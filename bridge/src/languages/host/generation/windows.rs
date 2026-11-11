//! Read identity and change time from the same metadata-only Windows handle.
use std::{
    fs::{File, Metadata, OpenOptions},
    os::windows::{fs::OpenOptionsExt, io::AsRawHandle},
    path::Path,
};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_BASIC_INFO, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_ID_INFO,
    FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, FileBasicInfo,
    FileIdInfo, GetFileInformationByHandleEx,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::languages::host) struct Identity {
    volume: u64,
    file: [u8; 16],
    changed: i64,
    born: i64,
}
impl Identity {
    pub(in crate::languages::host) fn fingerprint(&self) -> String {
        let mut bytes = Vec::with_capacity(32);
        bytes.extend_from_slice(&self.volume.to_le_bytes());
        bytes.extend_from_slice(&self.file);
        bytes.extend_from_slice(&self.born.to_le_bytes());
        format!("w1.{}", openwebide_core::plugins::content_digest(&bytes))
    }
}
pub(super) fn open(path: &Path) -> Result<File, String> {
    OpenOptions::new()
        .access_mode(FILE_READ_ATTRIBUTES)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
        .map_err(|error| error.to_string())
}

pub(in crate::languages::host) fn read(path: &Path) -> Result<(Metadata, Identity), String> {
    from_file(&open(path)?)
}

fn file_id(file: &File) -> Result<FILE_ID_INFO, String> {
    let mut identity = FILE_ID_INFO::default();
    // The synchronous call writes only this live SDK-layout output buffer.
    if unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle(),
            FileIdInfo,
            std::ptr::from_mut(&mut identity).cast(),
            u32::try_from(std::mem::size_of::<FILE_ID_INFO>()).unwrap(),
        )
    } == 0
    {
        return Err(std::io::Error::last_os_error().to_string());
    }
    Ok(identity)
}

pub(super) fn same_object(left: &File, right: &File) -> Result<bool, String> {
    let left = file_id(left)?;
    let right = file_id(right)?;
    Ok(left.VolumeSerialNumber == right.VolumeSerialNumber
        && left.FileId.Identifier == right.FileId.Identifier)
}

pub(super) fn from_file(file: &File) -> Result<(Metadata, Identity), String> {
    let mut basic = FILE_BASIC_INFO::default();
    // Both output buffers have the exact SDK layouts and remain live for these
    // synchronous calls. The handle is owned and shared for rename/delete.
    let basic_ok = unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle(),
            FileBasicInfo,
            std::ptr::from_mut(&mut basic).cast(),
            u32::try_from(std::mem::size_of::<FILE_BASIC_INFO>()).unwrap(),
        )
    };
    if basic_ok == 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    let identity = file_id(file)?;
    let metadata = file.metadata().map_err(|error| error.to_string())?;
    let mut after = FILE_BASIC_INFO::default();
    // A second change-time observation rejects modifications spanning the
    // identity/metadata reads, while retaining the same owned object handle.
    if unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle(),
            FileBasicInfo,
            std::ptr::from_mut(&mut after).cast(),
            u32::try_from(std::mem::size_of::<FILE_BASIC_INFO>()).unwrap(),
        )
    } == 0
    {
        return Err(std::io::Error::last_os_error().to_string());
    }
    if basic.ChangeTime != after.ChangeTime {
        return Err("A project file changed while reading its identity".into());
    }
    Ok((
        metadata,
        Identity {
            volume: identity.VolumeSerialNumber,
            file: identity.FileId.Identifier,
            changed: basic.ChangeTime,
            born: basic.CreationTime,
        },
    ))
}
