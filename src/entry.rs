use async_fs::File;
use binrw::BinRead;
use futures_lite::io::{SeekFrom, Take};
use futures_lite::{ready, AsyncRead, AsyncReadExt, AsyncSeekExt, FutureExt};
use std::borrow::Cow;
use std::io::{Error, Read};
use std::path::PathBuf;
use std::sync::Arc;
use std::task::Poll;

/// An entry in the VPK.
#[derive(Debug)]
pub struct VPKEntry {
    /// [`VPKDirectoryEntry`].
    pub dir_entry: VPKDirectoryEntry,
    /// [`PathBuf`] to archive (VPK) to read from.
    ///
    /// Is [`Some`] when data for the entry must be read from the file
    /// (in addition to [`Self::preload_data`]).
    ///
    /// Is [`None`] when the data must be read only from
    /// [`Self::preload_data`].
    pub archive_path: Option<Arc<PathBuf>>,
    /// Preloaded data of the entry. This is read first before reading
    /// from the archive.
    pub preload_data: Vec<u8>,
}

impl VPKEntry {
    /// Get the data of the [`VPKEntry`].
    pub async fn get(&self) -> Result<Cow<[u8]>, Error> {
        let mut reader = self.reader().await?;
        let mut buf = Vec::new();
        reader.read_to_end(&mut buf).await?;
        Ok(Cow::from(buf))
    }

    /// Create a [`VPKEntryReader`].
    pub async fn reader(&self) -> Result<VPKEntryReader<'_>, Error> {
        let Some(path) = self.archive_path.as_ref() else {
            return Ok(VPKEntryReader::new(&self.preload_data, None));
        };

        let mut file = File::open(path.as_path()).await?;
        file.seek(SeekFrom::Start(self.dir_entry.archive_offset as u64))
            .await?;
        let file = file.take(self.dir_entry.file_length as u64);

        Ok(VPKEntryReader::new(&self.preload_data, Some(file)))
    }
}

/// A reader over the [`VPKEntry`].
pub enum VPKEntryReader<'a> {
    /// Only preloaded data must be read.
    PreloadedOnly {
        preloaded_data: std::io::Cursor<&'a [u8]>,
    },
    /// Read from preloaded data first and then the file.
    PreloadAndFile {
        /// Length of the preloaded data.
        preloaded_data_len: usize,
        /// Number of bytes of the preloaded data read so far.
        preloaded_bytes_read: usize,
        /// Preloaded data.
        preloaded_data: std::io::Cursor<&'a [u8]>,
        /// The file that must be read.
        file: Take<File>,
    },
    /// Only the file must be read.
    FileOnly { file: Take<File> },
}

impl AsyncRead for VPKEntryReader<'_> {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut [u8],
    ) -> Poll<std::io::Result<usize>> {
        match self.get_mut() {
            VPKEntryReader::PreloadedOnly { preloaded_data } => {
                Poll::Ready(preloaded_data.read(buf))
            }
            VPKEntryReader::PreloadAndFile {
                preloaded_data_len,
                preloaded_bytes_read,
                preloaded_data,
                file,
            } => {
                if preloaded_bytes_read >= preloaded_data_len {
                    let res = ready!(file.read(buf).poll(cx));
                    Poll::Ready(res)
                } else {
                    let bytes_read = preloaded_data.read(buf)?;

                    let bytes_read = if bytes_read < buf.len() {
                        let file_bytes_read = ready!(file.read(&mut buf[bytes_read..]).poll(cx))?;
                        bytes_read + file_bytes_read
                    } else {
                        bytes_read
                    };

                    *preloaded_bytes_read += bytes_read;

                    Poll::Ready(Ok(bytes_read))
                }
            }
            VPKEntryReader::FileOnly { file } => {
                let bytes_read = ready!(file.read(buf).poll(cx));
                Poll::Ready(bytes_read)
            }
        }
    }
}

impl<'a> VPKEntryReader<'a> {
    /// Create a new [`VPKEntryReader`].
    pub fn new(preloaded_data: &'a [u8], file: Option<Take<File>>) -> Self {
        match file {
            Some(file) => {
                if preloaded_data.is_empty() {
                    Self::FileOnly { file }
                } else {
                    Self::PreloadAndFile {
                        preloaded_data_len: preloaded_data.len(),
                        preloaded_bytes_read: 0,
                        preloaded_data: std::io::Cursor::new(preloaded_data),
                        file,
                    }
                }
            }
            None => Self::PreloadedOnly {
                preloaded_data: std::io::Cursor::new(preloaded_data),
            },
        }
    }
}

/// [`VPKEntry`] header.
///
/// Information about the entry stored in the root VPK.
#[derive(Debug, BinRead)]
pub struct VPKDirectoryEntry {
    /// 32 bit CRC.
    pub crc32: u32,
    /// Number of bytes to preload from the root VPK.
    pub preload_length: u16,
    /// Index of archive to load entry from.
    pub archive_index: u16,
    /// Offset of the entry in the archive.
    pub archive_offset: u32,
    /// Length of the entry in the archive.
    ///
    /// # Note
    ///
    /// This does not include the [`Self::preload_length`]. Thus the
    /// total entry length would be [`Self::preload_length`] +
    /// [`Self::file_length`].
    pub file_length: u32,
    /// Suffix of the header. This seems to be used for ensuring the
    /// entry is read correctly from the root VPK.
    pub suffix: u16,
}
