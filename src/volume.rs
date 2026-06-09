//! Windows-only volume FFI: open a raw NTFS volume, query its geometry, and
//! bulk-read the `$MFT` into memory.
//!
//! Ported from the WinDirStat `FinderNtfsContext::LoadRoot` recipe
//! (references/windirstat/windirstat/FinderNtfs.cpp): open `\\.\C:`, query
//! `FSCTL_GET_NTFS_VOLUME_DATA` for the record/cluster geometry, ask the
//! filesystem for the `$MFT`'s already-resolved extents via
//! `FSCTL_GET_RETRIEVAL_POINTERS`, then read each extent off the volume and
//! concatenate them in VCN order into a contiguous MFT image.
//!
//! Requires Administrator. Non-elevated callers fail cleanly at `CreateFileW`
//! with an access-denied `io::Error`.
//!
//! Buffering decision: we deliberately do NOT use `FILE_FLAG_NO_BUFFERING`.
//! That flag would require every read offset/length and the destination buffer
//! to be sector-aligned; a `Vec<u8>` is only 8-byte aligned. A normal buffered
//! read of a raw volume handle has no alignment constraint and is plenty fast
//! for this spike, so we trade a little throughput for much simpler, safer code.

use std::io;

use windows::Win32::Foundation::{CloseHandle, ERROR_MORE_DATA, HANDLE};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, ReadFile, SetFilePointerEx, FILE_BEGIN, FILE_FLAGS_AND_ATTRIBUTES,
    FILE_READ_ATTRIBUTES, FILE_READ_DATA, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    OPEN_EXISTING, SYNCHRONIZE,
};
use windows::Win32::System::Ioctl::{
    NTFS_VOLUME_DATA_BUFFER, RETRIEVAL_POINTERS_BUFFER, STARTING_VCN_INPUT_BUFFER,
    FSCTL_GET_NTFS_VOLUME_DATA, FSCTL_GET_RETRIEVAL_POINTERS,
};
use windows::Win32::System::IO::DeviceIoControl;
use windows::core::PCWSTR;

/// Geometry + raw MFT bytes for a volume.
pub struct MftImage {
    /// The `$MFT` data, extents concatenated in VCN order — a contiguous MFT image.
    pub bytes: Vec<u8>,
    /// `BytesPerFileRecordSegment` (usually 1024).
    pub record_size: usize,
}

/// RAII wrapper so the volume/file handles are always closed, even on early return.
struct OwnedHandle(HANDLE);

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        // SAFETY: handle came from a successful CreateFileW and is closed exactly once.
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// Convert a Rust `&str` into a NUL-terminated UTF-16 buffer for `PCWSTR`.
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Open `\\.\<drive>:`, read NTFS geometry and the (possibly fragmented) `$MFT`
/// into memory. Requires Administrator. `drive` is a bare letter like `"C"`.
pub fn read_mft(drive: &str) -> io::Result<MftImage> {
    // Normalize the drive argument to a single letter, e.g. "c:", "C:\" -> "C".
    let letter = drive.trim().trim_end_matches([':', '\\', '/']);
    if letter.len() != 1 || !letter.chars().next().unwrap().is_ascii_alphabetic() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("invalid drive specifier: {drive:?} (expected a letter like \"C\")"),
        ));
    }
    let letter = letter.to_ascii_uppercase();

    // 1. Open the volume device. FILE_FLAG_NO_BUFFERING intentionally omitted
    //    (see module docs); reads are ordinary buffered synchronous reads.
    let volume_path = wide(&format!("\\\\.\\{letter}:"));
    // SAFETY: volume_path is a valid NUL-terminated UTF-16 buffer that outlives the call.
    let volume = unsafe {
        CreateFileW(
            PCWSTR(volume_path.as_ptr()),
            FILE_READ_DATA.0 | FILE_READ_ATTRIBUTES.0 | SYNCHRONIZE.0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            None,
            OPEN_EXISTING,
            FILE_FLAGS_AND_ATTRIBUTES(0),
            None,
        )
    }
    .map_err(to_io)?;
    let volume = OwnedHandle(volume);

    // 2. Query NTFS geometry: bytes per cluster, bytes per file record segment.
    let mut vol_data = NTFS_VOLUME_DATA_BUFFER::default();
    let mut returned: u32 = 0;
    // SAFETY: vol_data is a valid, correctly-sized output buffer for this FSCTL.
    unsafe {
        DeviceIoControl(
            volume.0,
            FSCTL_GET_NTFS_VOLUME_DATA,
            None,
            0,
            Some((&mut vol_data as *mut NTFS_VOLUME_DATA_BUFFER).cast()),
            std::mem::size_of::<NTFS_VOLUME_DATA_BUFFER>() as u32,
            Some(&mut returned),
            None,
        )
    }
    .map_err(to_io)?;

    let bytes_per_cluster = vol_data.BytesPerCluster as u64;
    let record_size = vol_data.BytesPerFileRecordSegment as usize;
    if bytes_per_cluster == 0 || record_size == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "NTFS volume reported zero cluster/record size (not an NTFS volume?)",
        ));
    }

    // 3. Open the $MFT's $DATA stream and ask the filesystem for its extents.
    //    FSCTL_GET_RETRIEVAL_POINTERS returns the already-resolved runlist, so we
    //    don't have to decode NTFS mapping pairs ourselves.
    let mft_path = wide(&format!("\\\\.\\{letter}:\\$MFT::$DATA"));
    // SAFETY: mft_path is a valid NUL-terminated UTF-16 buffer that outlives the call.
    let mft = unsafe {
        CreateFileW(
            PCWSTR(mft_path.as_ptr()),
            FILE_READ_ATTRIBUTES.0 | SYNCHRONIZE.0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            None,
            OPEN_EXISTING,
            FILE_FLAGS_AND_ATTRIBUTES(0),
            None,
        )
    }
    .map_err(to_io)?;
    let mft = OwnedHandle(mft);

    let extents = read_mft_extents(&mft)?;

    // 4 + 5 + 6. Read each extent off the volume and concatenate in VCN order.
    let mut bytes: Vec<u8> = Vec::new();
    const CHUNK: u64 = 4 * 1024 * 1024; // 4 MiB
    for (start_lcn, cluster_count) in extents {
        let mut remaining = cluster_count.saturating_mul(bytes_per_cluster);
        let mut offset = start_lcn.saturating_mul(bytes_per_cluster);

        while remaining > 0 {
            let this = remaining.min(CHUNK);
            let start = bytes.len();
            bytes.resize(start + this as usize, 0);

            // Position the volume handle at the absolute byte offset of this chunk.
            // SAFETY: volume handle is valid; lpnewfilepointer is None.
            unsafe { SetFilePointerEx(volume.0, offset as i64, None, FILE_BEGIN) }
                .map_err(to_io)?;

            let mut bytes_read: u32 = 0;
            // SAFETY: the destination slice is a freshly-sized region of `bytes`;
            // ReadFile infers the count from the slice length.
            unsafe {
                ReadFile(
                    volume.0,
                    Some(&mut bytes[start..start + this as usize]),
                    Some(&mut bytes_read),
                    None,
                )
            }
            .map_err(to_io)?;

            if bytes_read == 0 {
                // Unexpected short read; stop to avoid an infinite loop.
                bytes.truncate(start);
                break;
            }

            // Trim if the OS returned fewer bytes than requested.
            if (bytes_read as u64) < this {
                bytes.truncate(start + bytes_read as usize);
            }
            remaining -= bytes_read as u64;
            offset += bytes_read as u64;
        }
    }

    if bytes.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "read zero bytes for $MFT (no extents?)",
        ));
    }

    Ok(MftImage { bytes, record_size })
}

/// Run `FSCTL_GET_RETRIEVAL_POINTERS` on the `$MFT` handle, growing the output
/// buffer while it reports `ERROR_MORE_DATA`, and decode the extents into
/// `(start_lcn, cluster_count)` pairs in VCN order.
fn read_mft_extents(mft: &OwnedHandle) -> io::Result<Vec<(u64, u64)>> {
    let input = STARTING_VCN_INPUT_BUFFER { StartingVcn: 0 };

    // Start with room for 32 extents and double on ERROR_MORE_DATA.
    let header = std::mem::size_of::<RETRIEVAL_POINTERS_BUFFER>();
    let extent = std::mem::size_of::<windows::Win32::System::Ioctl::RETRIEVAL_POINTERS_BUFFER_0>();
    let mut buf: Vec<u8> = vec![0u8; header + 32 * extent];

    loop {
        let mut returned: u32 = 0;
        // SAFETY: input is a valid in-buffer; buf is a valid out-buffer of the
        // size we pass. The FSCTL writes a RETRIEVAL_POINTERS_BUFFER + extents.
        let result = unsafe {
            DeviceIoControl(
                mft.0,
                FSCTL_GET_RETRIEVAL_POINTERS,
                Some((&input as *const STARTING_VCN_INPUT_BUFFER).cast()),
                std::mem::size_of::<STARTING_VCN_INPUT_BUFFER>() as u32,
                Some(buf.as_mut_ptr().cast()),
                buf.len() as u32,
                Some(&mut returned),
                None,
            )
        };

        match result {
            Ok(()) => break,
            Err(e) => {
                // ERROR_MORE_DATA: the runlist didn't fit; the returned data is a
                // valid prefix but we want the whole thing, so grow and retry.
                if e.code() == windows::core::HRESULT::from_win32(ERROR_MORE_DATA.0) {
                    buf.resize(buf.len() * 2, 0);
                    continue;
                }
                return Err(to_io(e));
            }
        }
    }

    // Decode: the header is followed by `ExtentCount` {NextVcn, Lcn} pairs.
    // SAFETY: the FSCTL succeeded and wrote a valid RETRIEVAL_POINTERS_BUFFER
    // into `buf`; we only read ExtentCount entries from the same buffer.
    let hdr = unsafe { &*(buf.as_ptr() as *const RETRIEVAL_POINTERS_BUFFER) };
    let count = hdr.ExtentCount as usize;
    let mut prev_vcn = hdr.StartingVcn;

    let mut extents = Vec::with_capacity(count);
    for i in 0..count {
        // Each extent record lives at header_offset + i * sizeof(extent). We use
        // the field offset of `Extents[0]` within the struct as the base.
        let ext_base = std::mem::offset_of!(RETRIEVAL_POINTERS_BUFFER, Extents);
        let off = ext_base + i * extent;
        // SAFETY: i < ExtentCount and the buffer held ExtentCount extents.
        let ext = unsafe {
            &*(buf.as_ptr().add(off)
                as *const windows::Win32::System::Ioctl::RETRIEVAL_POINTERS_BUFFER_0)
        };
        let cluster_count = (ext.NextVcn - prev_vcn).max(0) as u64;
        let start_lcn = ext.Lcn;
        prev_vcn = ext.NextVcn;

        // Lcn == -1 marks a sparse/unallocated run; skip it (shouldn't occur for $MFT).
        if start_lcn < 0 {
            continue;
        }
        extents.push((start_lcn as u64, cluster_count));
    }

    if extents.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "FSCTL_GET_RETRIEVAL_POINTERS returned no usable extents for $MFT",
        ));
    }
    Ok(extents)
}

/// Map a `windows_core::Error` into a `std::io::Error`, preserving the raw
/// Win32 code where possible so callers see e.g. "Access is denied".
fn to_io(e: windows::core::Error) -> io::Error {
    match e.code().0 as u32 & 0xFFFF {
        // HRESULTs from from_win32 are 0x8007xxxx; recover the bare Win32 code.
        code if (e.code().0 as u32) & 0xFFFF_0000 == 0x8007_0000 => {
            io::Error::from_raw_os_error(code as i32)
        }
        _ => io::Error::other(e.to_string()),
    }
}
