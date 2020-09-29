use std::ffi::OsStr;
use std::fmt;
use std::mem::size_of;
use std::os::windows::ffi::OsStrExt;
use std::ptr;

use anyhow::{anyhow, Result};
use winapi::ctypes::c_void;
use winapi::shared::minwindef::{DWORD, UINT};
use winapi::um::winver::{GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW};

/// VS_FIXEDFILEINFO, C version
#[repr(C)]
#[allow(non_snake_case)]
#[derive(Copy, Clone, Debug, Default)]
struct RawFixedFileInfo {
    pub dwSignature: DWORD,
    pub dwStrucVersion: DWORD,
    pub dwFileVersionMS: DWORD,
    pub dwFileVersionLS: DWORD,
    pub dwProductVersionMS: DWORD,
    pub dwProductVersionLS: DWORD,
    pub dwFileFlagsMask: DWORD,
    pub dwFileFlags: DWORD,
    pub dwFileOS: DWORD,
    pub dwFileType: DWORD,
    pub dwFileSubtype: DWORD,
    pub dwFileDateMS: DWORD,
    pub dwFileDateLS: DWORD,
}

/// A Rust representation of [VS_FIXEDFILEINFO].
///
/// [VS_FIXEDFILEINFO] https://docs.microsoft.com/en-us/windows/win32/api/verrsrc/ns-verrsrc-vs_fixedfileinfo
#[derive(Copy, Clone, Debug, Default)]
pub struct FixedFileInfo {
    /// The binary version number of this structure
    pub struc_version: u32,
    /// The file's binary version number. Combined dwFileVersionMS and dwFileVersionLS
    pub file_version: u64,
    /// The product's binary version number. Combined dwProductVersionMS and dwProductVersionLS
    pub product_version: u64,
    /// Bitmask of valid bits in file_flags
    pub file_flags_mask: u32,
    /// File flags
    pub file_flags: u32,
    /// The operating system for which this file was designed
    pub file_os: u32,
    /// The general type of the file.
    pub file_type: u32,
    /// The function of the file. The possible values depend on the value of file_type.
    pub file_subtype: u32,
    /// The binary creation create date & time stamp. Combined dwFileDateMS and dwFileDateLS
    pub file_date: u64,
}

impl From<RawFixedFileInfo> for FixedFileInfo {
    #[rustfmt::skip]
    fn from(r: RawFixedFileInfo) -> Self {
        #[inline]
        fn combine_dwords(high: u32, low: u32) -> u64 {
            ((high as u64) << 32) | (low as u64)
        }

        Self {
            struc_version:   r.dwSignature,
            file_version:    combine_dwords(r.dwFileVersionMS, r.dwFileVersionLS),
            product_version: combine_dwords(r.dwProductVersionMS, r.dwProductVersionLS),
            file_flags_mask: r.dwFileFlagsMask,
            file_flags:      r.dwFileFlags,
            file_os:         r.dwFileOS,
            file_type:       r.dwFileType,
            file_subtype:    r.dwFileSubtype,
            file_date:       combine_dwords(r.dwFileDateMS, r.dwFileDateLS),
        }
    }
}

/// A 4-part version number, usually unpacked from file_version or product_version fields of
/// FixedFileInfo. A u64 is treated as 4 16-bit components. Version.0 is the highest 16 bits and
/// Version.3 is the lowest 16 bits.
#[derive(Debug, Copy, Clone, Default)]
pub struct Version(u16, u16, u16, u16);

impl From<u64> for Version {
    fn from(n: u64) -> Self {
        Self(
            ((n >> 48) & 0xffff) as u16,
            ((n >> 32) & 0xffff) as u16,
            ((n >> 16) & 0xffff) as u16,
            (n & 0xffff) as u16,
        )
    }
}

impl From<Version> for u64 {
    fn from(v: Version) -> u64 {
        ((v.0 as u64) << 48) | ((v.1 as u64) << 32) | ((v.2 as u64) << 16) | (v.3 as u64)
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}.{}.{}.{}", self.0, self.1, self.2, self.3)
    }
}

/// convert an OsStr (which is encoded as UTF-8 like "WTF-8") into a null-terminated Win32 Wide
/// String, a pointer to which can serve as a LPCWSTR.
/// Returns a Vec<u16> of the encoded and null-terminated string, or an Error indicating that there
/// was an inner null byte in the source, which is illegal. The error value is the index of the
/// first inner null byte.
// TODO: return an actual error object
fn to_wide_string<S>(s: &S) -> Result<Vec<u16>>
where
    S: AsRef<OsStr> + ?Sized,
{
    // OsStrExt::encode_wide gives us a u16 iterator
    let mut v: Vec<u16> = s.as_ref().encode_wide().collect();
    // check for inner null bytes
    for (i, c) in v.iter().enumerate() {
        if *c == 0 {
            return Err(anyhow!("inner nullbyte at position {}", i));
        }
    }
    // append null terminator
    v.push(0);
    Ok(v)
}

/// Call GetFileVersionInfoW and return the raw data buffer as a boxed slice
fn get_version_data<S>(path: &S) -> Result<Box<[u8]>>
where
    S: AsRef<OsStr> + ?Sized,
{
    let path_w = to_wide_string(path.as_ref())?;
    let mut handle: DWORD = 0;
    // DWORD GetFileVersionInfoSizeW(LPCWSTR lptstrFilename, LPDWORD lpdwHandle);
    let size = unsafe { GetFileVersionInfoSizeW(path_w.as_ptr(), &mut handle) };
    if size == 0 {
        return Err(anyhow!("GetFileVersionInfoSizeW failed"));
    }

    let mut buf = vec![0u8; size as usize];
    // BOOL GetFileVersionInfoW(LPCWSTR lptstrFilename, DWORD dwHandle, DWORD dwLen, LPVOID lpData);
    // dwhandle is ignored.
    // Safety: lpData must be valid for dwLen bytes
    let ret = unsafe { GetFileVersionInfoW(path_w.as_ptr(), 0, size, buf.as_mut_ptr() as *mut _) };
    match ret {
        0 => Err(anyhow!("GetFileVersionInfoW failed")),
        _ => Ok(buf.into_boxed_slice()),
    }
}

/// Call VerQueryValueW to vet the root-block FixedFileInfo data.
///
/// Safety: vdata must contain data that was returned successfully from GetFileVersionInfoW.
/// A pointer to vdata will be passed to VerQueryValue with no size checking.
unsafe fn get_fixed_info(vdata: &[u8]) -> Result<FixedFileInfo> {
    let mut pinfo: *mut c_void = ptr::null_mut();
    let mut pinfo_size: UINT = 0;
    let block = to_wide_string("\\")?;

    // BOOL VerQueryValue(LPCVOID pBlock, LPCWSTR lpSubBlock, LPVOID *lplpBuffer, PUINT puLen);
    // pBlock is the data returned by GetFileVersionInfoW.
    // lpSubBlock is the block to read. \ is the root block
    // lplpBuffer is a void** output pointer
    // puLen is the length of *lplpBuffer
    //
    // Safety: pinfo points somewhere inside vdata, don't let it outlive this function
    let ret = VerQueryValueW(
        vdata.as_ptr() as *const _,
        block.as_ptr(),
        &mut pinfo,
        &mut pinfo_size,
    );

    // error checking
    if ret == 0 {
        return Err(anyhow!("VerQueryValueW failed"));
    }
    if pinfo.is_null() {
        return Err(anyhow!("Got null result from VerQueryValueA"));
    }
    if (pinfo_size as usize) < size_of::<RawFixedFileInfo>() {
        return Err(anyhow!(
            "Not enough RawFixedFileInfo data. Expected {} got {}",
            size_of::<RawFixedFileInfo>(),
            pinfo_size
        ));
    }

    // safety: use an unaligned write because we don't know for sure that the raw block is properly
    // aligned. Maybe this is excess paranoia?
    let raw_info = ptr::read_unaligned(pinfo as *const RawFixedFileInfo);

    // the signature is supposed to be this magic number
    if raw_info.dwSignature != 0xfeef04bd {
        return Err(anyhow!(
            "Unexpected VS_FILEINFO signature {:x}",
            raw_info.dwSignature
        ));
    }

    Ok(FixedFileInfo::from(raw_info))
}

/// Get the root-level fixed info for a file
pub fn get_file_fixed_info<S>(path: &S) -> Result<FixedFileInfo>
where
    S: AsRef<OsStr> + ?Sized,
{
    let data = get_version_data(path)?;
    unsafe { get_fixed_info(&data) }
}
