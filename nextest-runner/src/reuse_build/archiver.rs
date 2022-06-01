// Copyright (c) The nextest Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use super::{ArchiveEvent, BINARIES_METADATA_FILE_NAME, CARGO_METADATA_FILE_NAME};
use crate::{
    errors::{ArchiveCreateError, UnknownArchiveFormat},
    helpers::convert_rel_path_to_forward_slash,
    list::{BinaryList, OutputFormat, SerializableFormat},
    reuse_build::PathMapper,
};
use atomicwrites::{AtomicFile, OverwriteBehavior};
use camino::Utf8Path;
use std::{
    io::{self, BufWriter, Write},
    time::{Instant, SystemTime},
};
use zstd::Encoder;

/// Archive format.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ArchiveFormat {
    /// A Zstandard-compressed tarball.
    TarZst,
}

impl ArchiveFormat {
    /// The list of supported formats as a list of (file extension, format) pairs.
    pub const SUPPORTED_FORMATS: &'static [(&'static str, Self)] = &[(".tar.zst", Self::TarZst)];

    /// Automatically detects an archive format from a given file name, and returns an error if the
    /// detection failed.
    pub fn autodetect(archive_file: &Utf8Path) -> Result<Self, UnknownArchiveFormat> {
        let file_name = archive_file.file_name().unwrap_or("");
        for (extension, format) in Self::SUPPORTED_FORMATS {
            if file_name.ends_with(extension) {
                return Ok(*format);
            }
        }

        Err(UnknownArchiveFormat {
            file_name: file_name.to_owned(),
        })
    }
}

/// Archives test binaries along with metadata to the given file.
///
/// The output file is a Zstandard-compressed tarball (`.tar.zst`).
pub fn archive_to_file<'a, F>(
    binary_list: &'a BinaryList,
    cargo_metadata: &'a str,
    path_mapper: &'a PathMapper,
    format: ArchiveFormat,
    zstd_level: i32,
    output_file: &'a Utf8Path,
    mut callback: F,
) -> Result<(), ArchiveCreateError>
where
    F: FnMut(ArchiveEvent<'a>) -> io::Result<()>,
{
    let file = AtomicFile::new(output_file, OverwriteBehavior::AllowOverwrite);
    let test_binary_count = binary_list.rust_binaries.len();
    let non_test_binary_count = binary_list.rust_build_meta.non_test_binaries.len();
    let linked_path_count = binary_list.rust_build_meta.linked_paths.len();
    let start_time = Instant::now();

    let file_count = file
        .write(|file| {
            callback(ArchiveEvent::ArchiveStarted {
                test_binary_count,
                non_test_binary_count,
                linked_path_count,
                output_file,
            })
            .map_err(ArchiveCreateError::ReporterIo)?;
            // Write out the archive.
            let archiver = Archiver::new(
                binary_list,
                cargo_metadata,
                path_mapper,
                format,
                zstd_level,
                file,
            )?;
            let (_, file_count) = archiver.archive()?;
            Ok(file_count)
        })
        .map_err(|err| match err {
            atomicwrites::Error::Internal(err) => ArchiveCreateError::OutputArchiveIo(err),
            atomicwrites::Error::User(err) => err,
        })?;

    let elapsed = start_time.elapsed();

    callback(ArchiveEvent::Archived {
        file_count,
        output_file,
        elapsed,
    })
    .map_err(ArchiveCreateError::ReporterIo)?;

    Ok(())
}

struct Archiver<'a, W: Write> {
    binary_list: &'a BinaryList,
    cargo_metadata: &'a str,
    path_mapper: &'a PathMapper,
    builder: tar::Builder<Encoder<'static, BufWriter<W>>>,
    unix_timestamp: u64,
    file_count: usize,
}

impl<'a, W: Write> Archiver<'a, W> {
    fn new(
        binary_list: &'a BinaryList,
        cargo_metadata: &'a str,
        path_mapper: &'a PathMapper,
        format: ArchiveFormat,
        compression_level: i32,
        writer: W,
    ) -> Result<Self, ArchiveCreateError> {
        let buf_writer = BufWriter::new(writer);
        let builder = match format {
            ArchiveFormat::TarZst => {
                let mut encoder = zstd::Encoder::new(buf_writer, compression_level)
                    .map_err(ArchiveCreateError::OutputArchiveIo)?;
                encoder
                    .include_checksum(true)
                    .map_err(ArchiveCreateError::OutputArchiveIo)?;
                encoder
                    .multithread(num_cpus::get() as u32)
                    .map_err(ArchiveCreateError::OutputArchiveIo)?;
                tar::Builder::new(encoder)
            }
        };

        let unix_timestamp = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .expect("current time should be after 1970-01-01")
            .as_secs();

        Ok(Self {
            binary_list,
            cargo_metadata,
            path_mapper,
            builder,
            unix_timestamp,
            file_count: 0,
        })
    }

    fn archive(mut self) -> Result<(W, usize), ArchiveCreateError> {
        // Add the binaries metadata first so that while unarchiving, reports are instant.
        let binaries_metadata = self
            .binary_list
            .to_string(OutputFormat::Serializable(SerializableFormat::JsonPretty))
            .map_err(ArchiveCreateError::CreateBinaryList)?;

        self.append_from_memory(BINARIES_METADATA_FILE_NAME, &binaries_metadata)?;

        self.append_from_memory(CARGO_METADATA_FILE_NAME, self.cargo_metadata)?;

        // Write all discovered binaries into the archive.
        let target_dir = &self.binary_list.rust_build_meta.target_directory;

        for binary in &self.binary_list.rust_binaries {
            let rel_path = binary
                .path
                .strip_prefix(target_dir.parent().expect("target dir cannot be the root"))
                .expect("binary paths must be within target directory");
            let rel_path = convert_rel_path_to_forward_slash(rel_path);

            self.append_path(&binary.path, &rel_path)?;
            self.file_count += 1;
        }
        for non_test_binary in self
            .binary_list
            .rust_build_meta
            .non_test_binaries
            .iter()
            .flat_map(|(_, binaries)| binaries)
        {
            let src_path = self
                .binary_list
                .rust_build_meta
                .target_directory
                .join(&non_test_binary.path);
            let src_path = self.path_mapper.map_binary(src_path);

            let rel_path = Utf8Path::new("target").join(&non_test_binary.path);
            let rel_path = convert_rel_path_to_forward_slash(&rel_path);

            self.append_path(&src_path, &rel_path)?;
            self.file_count += 1;
        }

        // Write linked paths to the archive.
        for linked_path in &self.binary_list.rust_build_meta.linked_paths {
            // linked paths are e.g. debug/foo/bar. We need to prepend the target directory.
            let src_path = self
                .binary_list
                .rust_build_meta
                .target_directory
                .join(linked_path);
            let src_path = self.path_mapper.map_binary(src_path);

            let rel_path = Utf8Path::new("target").join(linked_path);
            let rel_path = convert_rel_path_to_forward_slash(&rel_path);
            self.append_dir_all(&rel_path, &src_path, true)?;
        }

        // TODO: add extra files.

        // Finish writing the archive.
        let encoder = self
            .builder
            .into_inner()
            .map_err(ArchiveCreateError::OutputArchiveIo)?;
        // Finish writing the zstd stream.
        let buf_writer = encoder
            .finish()
            .map_err(ArchiveCreateError::OutputArchiveIo)?;
        let writer = buf_writer
            .into_inner()
            .map_err(|err| ArchiveCreateError::OutputArchiveIo(err.into_error()))?;

        Ok((writer, self.file_count))
    }

    // ---
    // Helper methods
    // ---

    fn append_from_memory(&mut self, name: &str, contents: &str) -> Result<(), ArchiveCreateError> {
        let mut header = tar::Header::new_gnu();
        header.set_size(contents.len() as u64);
        header.set_mtime(self.unix_timestamp);
        header.set_mode(0o664);
        header.set_cksum();

        self.builder
            .append_data(&mut header, name, io::Cursor::new(contents))
            .map_err(ArchiveCreateError::OutputArchiveIo)?;
        self.file_count += 1;
        Ok(())
    }

    // Adapted from tar-rs's source, with tracking for file counts and better error messages.
    fn append_dir_all(
        &mut self,
        rel_path: &Utf8Path,
        src_path: &Utf8Path,
        follow: bool,
    ) -> Result<(), ArchiveCreateError> {
        let mut stack = vec![(src_path.to_path_buf(), true, false)];

        while let Some((src, is_dir, is_symlink)) = stack.pop() {
            let dest = rel_path.join(src.strip_prefix(&src_path).unwrap());
            // In case of a symlink pointing to a directory, is_dir is false, but src.is_dir() will return true
            if is_dir || (is_symlink && follow && src.is_dir()) {
                for entry in
                    src.read_dir_utf8()
                        .map_err(|error| ArchiveCreateError::InputFileRead {
                            path: src.clone(),
                            is_dir: Some(true),
                            error,
                        })?
                {
                    let entry = entry.map_err(|error| ArchiveCreateError::DirEntryRead {
                        path: src.clone(),
                        error,
                    })?;
                    let file_name = entry.path();
                    let file_type =
                        entry
                            .file_type()
                            .map_err(|error| ArchiveCreateError::InputFileRead {
                                path: file_name.to_owned(),
                                is_dir: None,
                                error,
                            })?;
                    stack.push((
                        entry.path().to_path_buf(),
                        file_type.is_dir(),
                        file_type.is_symlink(),
                    ));
                }
                // No need to append the directory entry to the tarball since we don't care about
                // its metadata.
            } else {
                self.append_path(&src, &dest)?;
            }
        }

        Ok(())
    }

    fn append_path(&mut self, src: &Utf8Path, dest: &Utf8Path) -> Result<(), ArchiveCreateError> {
        self.builder
            .append_path_with_name(src, dest)
            .map_err(|error| ArchiveCreateError::InputFileRead {
                path: src.to_owned(),
                is_dir: Some(false),
                error,
            })?;
        self.file_count += 1;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_archive_format_autodetect() {
        assert_eq!(
            ArchiveFormat::autodetect("foo.tar.zst".as_ref()).unwrap(),
            ArchiveFormat::TarZst,
        );
        assert_eq!(
            ArchiveFormat::autodetect("foo/bar.tar.zst".as_ref()).unwrap(),
            ArchiveFormat::TarZst,
        );
        ArchiveFormat::autodetect("foo".as_ref()).unwrap_err();
        ArchiveFormat::autodetect("/".as_ref()).unwrap_err();
    }
}