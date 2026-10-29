//! 最小 ZIP 写入器：只做"存储"（不压缩），够诊断包用。
//!
//! 为什么不拉一个 zip 依赖：诊断包里的东西是日志和 JSON，压缩率本来就高不了，
//! 而我们只需要三件事 —— 写本地文件头、写中央目录、写结束记录。手写一份
//! 一百来行、可以被单元测试逐字节校验，比多一个依赖更划算。
//!
//! 支持：多个文件、UTF-8 文件名（置通用标志位 11）、DOS 时间戳、CRC-32。
//! 不支持：目录项、ZIP64、加密、压缩。诊断包达不到 4 GiB 的上限。

use std::io::Write;

/// CRC-32（IEEE 802.3）查表。表在首次使用时生成。
struct Crc32 {
    table: [u32; 256],
}

impl Crc32 {
    fn new() -> Self {
        let mut table = [0u32; 256];
        for (index, slot) in table.iter_mut().enumerate() {
            let mut value = index as u32;
            for _ in 0..8 {
                value = if value & 1 == 1 {
                    0xEDB8_8320 ^ (value >> 1)
                } else {
                    value >> 1
                };
            }
            *slot = value;
        }
        Self { table }
    }

    fn update(&self, seed: u32, data: &[u8]) -> u32 {
        let mut crc = !seed;
        for byte in data {
            let index = ((crc ^ u32::from(*byte)) & 0xFF) as usize;
            crc = self.table[index] ^ (crc >> 8);
        }
        !crc
    }
}

/// 一条待写入的条目。
struct Entry {
    name: String,
    data: Vec<u8>,
    crc: u32,
    offset: u32,
}

/// ZIP 写入器。用完必须调用 [`ZipWriter::finish`] 才会写出中央目录。
pub struct ZipWriter {
    buffer: Vec<u8>,
    entries: Vec<Entry>,
    crc: Crc32,
}

impl Default for ZipWriter {
    fn default() -> Self {
        Self::new()
    }
}

impl ZipWriter {
    /// 新建一个空包。
    #[must_use]
    pub fn new() -> Self {
        Self {
            buffer: Vec::new(),
            entries: Vec::new(),
            crc: Crc32::new(),
        }
    }

    /// 加一个文件。名字用 `/` 分隔子目录，必须是相对路径。
    pub fn add(&mut self, name: &str, data: impl Into<Vec<u8>>) {
        let data = data.into();
        let crc = self.crc.update(0, &data);
        let offset = u32::try_from(self.buffer.len()).unwrap_or(u32::MAX);
        let size = u32::try_from(data.len()).unwrap_or(u32::MAX);
        let name_bytes = name.as_bytes();
        let name_len = u16::try_from(name_bytes.len()).unwrap_or(u16::MAX);

        // 本地文件头。
        self.buffer.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
        self.buffer.extend_from_slice(&20u16.to_le_bytes()); // 需要的版本 2.0
        self.buffer.extend_from_slice(&0x0800u16.to_le_bytes()); // UTF-8 名字
        self.buffer.extend_from_slice(&0u16.to_le_bytes()); // 存储，不压缩
        self.buffer.extend_from_slice(&0u16.to_le_bytes()); // 时间 00:00:00
        self.buffer.extend_from_slice(&0x21u16.to_le_bytes()); // 日期 1980-01-01
        self.buffer.extend_from_slice(&crc.to_le_bytes());
        self.buffer.extend_from_slice(&size.to_le_bytes());
        self.buffer.extend_from_slice(&size.to_le_bytes());
        self.buffer.extend_from_slice(&name_len.to_le_bytes());
        self.buffer.extend_from_slice(&0u16.to_le_bytes()); // 没有额外字段
        self.buffer.extend_from_slice(name_bytes);
        self.buffer.extend_from_slice(&data);

        self.entries.push(Entry {
            name: name.to_string(),
            data,
            crc,
            offset,
        });
    }

    /// 写完中央目录，返回整个包。
    #[must_use]
    pub fn finish(mut self) -> Vec<u8> {
        let directory_offset = u32::try_from(self.buffer.len()).unwrap_or(u32::MAX);
        for entry in &self.entries {
            let size = u32::try_from(entry.data.len()).unwrap_or(u32::MAX);
            let name_bytes = entry.name.as_bytes();
            let name_len = u16::try_from(name_bytes.len()).unwrap_or(u16::MAX);
            self.buffer.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
            self.buffer.extend_from_slice(&20u16.to_le_bytes()); // 创建版本
            self.buffer.extend_from_slice(&20u16.to_le_bytes()); // 需要版本
            self.buffer.extend_from_slice(&0x0800u16.to_le_bytes()); // UTF-8
            self.buffer.extend_from_slice(&0u16.to_le_bytes()); // 存储
            self.buffer.extend_from_slice(&0u16.to_le_bytes()); // 时间
            self.buffer.extend_from_slice(&0x21u16.to_le_bytes()); // 日期
            self.buffer.extend_from_slice(&entry.crc.to_le_bytes());
            self.buffer.extend_from_slice(&size.to_le_bytes());
            self.buffer.extend_from_slice(&size.to_le_bytes());
            self.buffer.extend_from_slice(&name_len.to_le_bytes());
            self.buffer.extend_from_slice(&0u16.to_le_bytes()); // 额外字段
            self.buffer.extend_from_slice(&0u16.to_le_bytes()); // 注释
            self.buffer.extend_from_slice(&0u16.to_le_bytes()); // 磁盘号
            self.buffer.extend_from_slice(&0u16.to_le_bytes()); // 内部属性
            self.buffer.extend_from_slice(&0u32.to_le_bytes()); // 外部属性
            self.buffer.extend_from_slice(&entry.offset.to_le_bytes());
            self.buffer.extend_from_slice(name_bytes);
        }
        let count = u16::try_from(self.entries.len()).unwrap_or(u16::MAX);
        let directory_size = u32::try_from(self.buffer.len())
            .unwrap_or(0)
            .saturating_sub(directory_offset);

        // 结束记录。
        self.buffer.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
        self.buffer.extend_from_slice(&0u16.to_le_bytes()); // 本磁盘号
        self.buffer.extend_from_slice(&0u16.to_le_bytes()); // 中央目录起始磁盘
        self.buffer.extend_from_slice(&count.to_le_bytes());
        self.buffer.extend_from_slice(&count.to_le_bytes());
        self.buffer.extend_from_slice(&directory_size.to_le_bytes());
        self.buffer
            .extend_from_slice(&directory_offset.to_le_bytes());
        self.buffer.extend_from_slice(&0u16.to_le_bytes()); // 注释长度
        self.buffer
    }

    /// 直接写进一个文件（先写临时文件再改名，避免半个包）。
    pub fn write_to(self, path: &std::path::Path) -> std::io::Result<()> {
        let bytes = self.finish();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let temporary = path.with_extension("zip.tmp");
        {
            let mut file = std::fs::File::create(&temporary)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
        }
        std::fs::rename(&temporary, path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read_u32(bytes: &[u8], at: usize) -> u32 {
        u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
    }

    #[test]
    fn crc32_matches_known_values() {
        let crc = Crc32::new();
        assert_eq!(crc.update(0, b""), 0);
        assert_eq!(crc.update(0, b"123456789"), 0xCBF4_3926);
        assert_eq!(crc.update(0, b"The quick brown fox"), 0xB745_74DE);
    }

    #[test]
    fn empty_archive_is_just_the_end_record() {
        let bytes = ZipWriter::new().finish();
        assert_eq!(bytes.len(), 22);
        assert_eq!(read_u32(&bytes, 0), 0x0605_4b50);
    }

    #[test]
    fn one_entry_has_local_header_and_directory() {
        let mut zip = ZipWriter::new();
        zip.add("logs/daemon.log", b"hello".to_vec());
        let bytes = zip.finish();
        assert_eq!(read_u32(&bytes, 0), 0x0403_4b50, "本地文件头签名");
        // 名字长度在偏移 26，额外字段在 28。
        let name_len = u16::from_le_bytes([bytes[26], bytes[27]]) as usize;
        assert_eq!(name_len, "logs/daemon.log".len());
        let name_at = 30;
        assert_eq!(
            &bytes[name_at..name_at + name_len],
            b"logs/daemon.log",
            "文件名原样写入"
        );
        let data_at = name_at + name_len;
        assert_eq!(&bytes[data_at..data_at + 5], b"hello");
        // 结尾一定有中央目录结束记录。
        let tail = bytes.len() - 22;
        assert_eq!(read_u32(&bytes, tail), 0x0605_4b50);
        assert_eq!(
            u16::from_le_bytes([bytes[tail + 10], bytes[tail + 11]]),
            1,
            "一个条目"
        );
    }

    #[test]
    fn two_entries_are_both_recorded() {
        let mut zip = ZipWriter::new();
        zip.add("a.txt", b"aaa".to_vec());
        zip.add("b/c.txt", b"ccc".to_vec());
        let bytes = zip.finish();
        let tail = bytes.len() - 22;
        assert_eq!(u16::from_le_bytes([bytes[tail + 10], bytes[tail + 11]]), 2);
    }

    #[test]
    fn write_to_creates_a_real_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out/bundle.zip");
        let mut zip = ZipWriter::new();
        zip.add("report.json", b"{}".to_vec());
        zip.write_to(&path).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(read_u32(&bytes, 0), 0x0403_4b50);
        assert!(
            !path.with_extension("zip.tmp").exists(),
            "临时文件应当被改名"
        );
    }
}
