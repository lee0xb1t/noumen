//! 最小 PE 解析：只读头与导出表，不做整映像缓冲。
//!
//! 用「读一个区间」的闭包作为内存来源，因此：
//!
//! * uapp 里闭包就是 `MemClient::read_virt(cr3, ...)`；
//! * 单测里闭包从一个 `Vec<u8>` 拷贝，不需要驱动/虚拟机。
//!
//! 只实现本工具用到的三件事：`SizeOfImage`、`TimeDateStamp`、按名字查导出 RVA。

/// 读取 guest 内存的回调：把 `va` 处 `out.len()` 字节写进 `out`。
pub type ReadAt<'a> = &'a mut dyn FnMut(u64, &mut [u8]) -> Result<(), String>;

/// 解析出来的头字段。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PeHeaders {
    pub image_size: u32,
    pub time_date_stamp: u32,
    pub export_rva: u32,
    pub export_size: u32,
    pub is_pe32_plus: bool,
}

const DOS_HEADER_SIZE: usize = 0x40;
const NT_HEADER_MAX: usize = 0x120;
pub const MAX_EXPORT_NAMES: usize = 4096;
const NAME_BUFFER: usize = 128;

fn u16_at(bytes: &[u8], offset: usize) -> Result<u16, String> {
    bytes
        .get(offset..offset + 2)
        .ok_or_else(|| String::from("PE 头越界"))?
        .try_into()
        .map(u16::from_le_bytes)
        .map_err(|_| String::from("PE 头越界"))
}

fn u32_at(bytes: &[u8], offset: usize) -> Result<u32, String> {
    bytes
        .get(offset..offset + 4)
        .ok_or_else(|| String::from("PE 头越界"))?
        .try_into()
        .map(u32::from_le_bytes)
        .map_err(|_| String::from("PE 头越界"))
}

/// 校验 `MZ` / `PE\0\0` 并返回头字段。
pub fn parse_headers(read: ReadAt<'_>, base: u64) -> Result<PeHeaders, String> {
    if base == 0 || base & 0xFFF != 0 {
        return Err(format!("映像基址未页对齐: {base:#x}"));
    }

    let mut dos = [0u8; DOS_HEADER_SIZE];
    read(base, &mut dos)?;

    if &dos[0..2] != b"MZ" {
        return Err(String::from("缺少 MZ 签名"));
    }

    let e_lfanew = u32_at(&dos, 0x3C)? as usize;

    if !(DOS_HEADER_SIZE..=0x1000).contains(&e_lfanew) {
        return Err(format!("e_lfanew 非法: {e_lfanew:#x}"));
    }

    let mut nt = [0u8; NT_HEADER_MAX];
    read(base + e_lfanew as u64, &mut nt)?;

    if &nt[0..4] != b"PE\0\0" {
        return Err(String::from("缺少 PE 签名"));
    }

    // FileHeader.TimeDateStamp 在签名 + 8。
    let time_date_stamp = u32_at(&nt, 8)?;

    let magic = u16_at(&nt, 0x18)?;
    let is_pe32_plus = match magic {
        0x20B => true,
        0x10B => false,
        other => return Err(format!("未知的 OptionalHeader.Magic: {other:#x}")),
    };

    // SizeOfImage 在 OptionalHeader + 0x38，PE32/PE32+ 相同。
    let image_size = u32_at(&nt, 0x18 + 0x38)?;

    let data_directory = if is_pe32_plus { 0x18 + 0x70 } else { 0x18 + 0x60 };
    let export_rva = u32_at(&nt, data_directory)?;
    let export_size = u32_at(&nt, data_directory + 4)?;

    Ok(PeHeaders {
        image_size,
        time_date_stamp,
        export_rva,
        export_size,
        is_pe32_plus,
    })
}

/// 在导出表里按名字查符号，返回 **RVA**（调用方自己加 `base`）。
///
/// 找不到返回 `Ok(None)`；表结构损坏返回 `Err`。名字比较用长度 + 逐字节。
pub fn export_rva(read: ReadAt<'_>, base: u64, name: &[u8]) -> Result<Option<u32>, String> {
    let headers = parse_headers(&mut *read, base)?;

    if headers.export_rva == 0 || headers.export_size == 0 {
        return Ok(None);
    }

    let export_base = base + headers.export_rva as u64;
    let mut export_dir = [0u8; 40];
    read(export_base, &mut export_dir)?;

    let number_of_functions = u32_at(&export_dir, 0x14)? as usize;
    let number_of_names = u32_at(&export_dir, 0x18)? as usize;
    let address_of_functions = u32_at(&export_dir, 0x1C)? as usize;
    let address_of_names = u32_at(&export_dir, 0x20)? as usize;
    let address_of_ordinals = u32_at(&export_dir, 0x24)? as usize;

    if number_of_names == 0 || number_of_functions == 0 {
        return Ok(None);
    }

    let limit = number_of_names.min(MAX_EXPORT_NAMES);

    for index in 0..limit {
        let mut name_rva_buf = [0u8; 4];
        read(
            base + (address_of_names + index * 4) as u64,
            &mut name_rva_buf,
        )?;

        let name_rva = u32::from_le_bytes(name_rva_buf) as u64;

        let mut buffer = [0u8; NAME_BUFFER];
        read(base + name_rva, &mut buffer)?;

        let found = buffer
            .iter()
            .position(|byte| *byte == 0)
            .map(|length| &buffer[..length])
            .unwrap_or(&buffer[..]);

        if found != name {
            continue;
        }

        let mut ordinal_buf = [0u8; 2];
        read(
            base + (address_of_ordinals + index * 2) as u64,
            &mut ordinal_buf,
        )?;

        let ordinal = u16::from_le_bytes(ordinal_buf) as usize;

        if ordinal >= number_of_functions {
            return Err(format!("导出序号越界: {ordinal}"));
        }

        let mut function_rva_buf = [0u8; 4];
        read(
            base + (address_of_functions + ordinal * 4) as u64,
            &mut function_rva_buf,
        )?;

        return Ok(Some(u32::from_le_bytes(function_rva_buf)));
    }

    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: u64 = 0x1_4000_0000;

    /// 构造一个只有 DOS/NT 头 + 导出表的合成映像。
    fn synthetic_image() -> Vec<u8> {
        let mut image = vec![0u8; 0x400];

        image[0..2].copy_from_slice(b"MZ");
        image[0x3C..0x40].copy_from_slice(&0x80u32.to_le_bytes());

        let nt = 0x80;
        image[nt..nt + 4].copy_from_slice(b"PE\0\0");
        image[nt + 8..nt + 12].copy_from_slice(&0x1122_3344u32.to_le_bytes());
        image[nt + 0x18..nt + 0x1A].copy_from_slice(&0x20Bu16.to_le_bytes());
        image[nt + 0x50..nt + 0x54].copy_from_slice(&0x2000u32.to_le_bytes());

        // DataDirectory[0] = 导出表
        image[nt + 0x88..nt + 0x8C].copy_from_slice(&0x200u32.to_le_bytes());
        image[nt + 0x8C..nt + 0x90].copy_from_slice(&0x100u32.to_le_bytes());

        let export = 0x200;
        image[export + 0x14..export + 0x18].copy_from_slice(&1u32.to_le_bytes()); // NumberOfFunctions
        image[export + 0x18..export + 0x1C].copy_from_slice(&1u32.to_le_bytes()); // NumberOfNames
        image[export + 0x1C..export + 0x20].copy_from_slice(&0x300u32.to_le_bytes());
        image[export + 0x20..export + 0x24].copy_from_slice(&0x320u32.to_le_bytes());
        image[export + 0x24..export + 0x28].copy_from_slice(&0x340u32.to_le_bytes());

        image[0x300..0x304].copy_from_slice(&0x1000u32.to_le_bytes());
        image[0x320..0x324].copy_from_slice(&0x360u32.to_le_bytes());
        image[0x340..0x342].copy_from_slice(&0u16.to_le_bytes());
        image[0x360..0x36E].copy_from_slice(b"NtBuildNumber\0");

        image
    }

    fn reader<'a>(image: &'a [u8]) -> impl FnMut(u64, &mut [u8]) -> Result<(), String> + 'a {
        move |va, out| {
            let offset = va
                .checked_sub(BASE)
                .ok_or_else(|| String::from("读越界"))? as usize;
            let end = offset
                .checked_add(out.len())
                .ok_or_else(|| String::from("读越界"))?;

            if end > image.len() {
                return Err(String::from("读越界"));
            }

            out.copy_from_slice(&image[offset..end]);
            Ok(())
        }
    }

    #[test]
    fn parses_headers() {
        let image = synthetic_image();
        let mut read = reader(&image);
        let headers = parse_headers(&mut read, BASE).unwrap();

        assert_eq!(headers.image_size, 0x2000);
        assert_eq!(headers.time_date_stamp, 0x1122_3344);
        assert!(headers.is_pe32_plus);
        assert_eq!(headers.export_rva, 0x200);
    }

    #[test]
    fn finds_export_by_name() {
        let image = synthetic_image();
        let mut read = reader(&image);

        assert_eq!(
            export_rva(&mut read, BASE, b"NtBuildNumber").unwrap(),
            Some(0x1000)
        );
        assert_eq!(export_rva(&mut read, BASE, b"Nope").unwrap(), None);
    }

    #[test]
    fn rejects_bad_images() {
        let mut image = synthetic_image();
        image[0] = b'X';

        let mut read = reader(&image);
        assert!(parse_headers(&mut read, BASE).is_err());

        let image = synthetic_image();
        let mut read = reader(&image);
        assert!(parse_headers(&mut read, BASE + 0x800).is_err());
    }
}
