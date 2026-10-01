use std::io;

pub(crate) fn be_u16(data: &[u8], at: usize) -> io::Result<u16> {
    let b = data
        .get(at..at + 2)
        .ok_or_else(|| invalid("truncated u16"))?;
    Ok(u16::from_be_bytes([b[0], b[1]]))
}

pub(crate) fn be_u24(data: &[u8], at: usize) -> io::Result<u32> {
    let b = data
        .get(at..at + 3)
        .ok_or_else(|| invalid("truncated u24"))?;
    Ok(((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32)
}

pub(crate) fn be_u32(data: &[u8], at: usize) -> io::Result<u32> {
    let b = data
        .get(at..at + 4)
        .ok_or_else(|| invalid("truncated u32"))?;
    Ok(u32::from_be_bytes(b.try_into().unwrap()))
}

pub(crate) fn be_u64(data: &[u8], at: usize) -> io::Result<u64> {
    let b = data
        .get(at..at + 8)
        .ok_or_else(|| invalid("truncated u64"))?;
    Ok(u64::from_be_bytes(b.try_into().unwrap()))
}

pub(crate) fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

pub(crate) fn invalid_at(pos: u64, message: impl Into<String>) -> io::Error {
    invalid(format!("at .MYD offset {pos}: {}", message.into()))
}

pub(crate) fn le_u16(data: &[u8], at: usize) -> io::Result<u16> {
    let b = data
        .get(at..at + 2)
        .ok_or_else(|| invalid("truncated little-endian u16"))?;
    Ok(u16::from_le_bytes([b[0], b[1]]))
}

pub(crate) fn le_u32(data: &[u8], at: usize) -> io::Result<u32> {
    let b = data
        .get(at..at + 4)
        .ok_or_else(|| invalid("truncated little-endian u32"))?;
    Ok(u32::from_le_bytes(b.try_into().unwrap()))
}

pub(crate) fn read_le_len(bytes: &[u8], pos: &mut usize, width: usize) -> io::Result<usize> {
    if width == 0 || width > 4 || pos.saturating_add(width) > bytes.len() {
        return Err(invalid("truncated length prefix"));
    }
    let mut len = 0usize;
    for (shift, b) in bytes[*pos..*pos + width].iter().enumerate() {
        len |= (*b as usize) << (shift * 8);
    }
    *pos += width;
    Ok(len)
}

pub(crate) fn read_varchar_len(bytes: &[u8], pos: &mut usize) -> io::Result<usize> {
    let first = *bytes
        .get(*pos)
        .ok_or_else(|| invalid("truncated VARCHAR length"))?;
    *pos += 1;
    if first != 255 {
        Ok(first as usize)
    } else {
        let b = bytes
            .get(*pos..*pos + 2)
            .ok_or_else(|| invalid("truncated extended VARCHAR length"))?;
        *pos += 2;
        // MyISAM's `mi_int2store` uses big endian for the extended key-length prefix.
        Ok(u16::from_be_bytes([b[0], b[1]]) as usize)
    }
}

pub(crate) fn read_packed_string_len(
    bytes: &[u8],
    pos: &mut usize,
    max: usize,
) -> io::Result<usize> {
    let first = *bytes
        .get(*pos)
        .ok_or_else(|| invalid("truncated packed string length"))?;
    *pos += 1;
    if max > 255 && first & 0x80 != 0 {
        let second = *bytes
            .get(*pos)
            .ok_or_else(|| invalid("truncated packed string length"))?;
        *pos += 1;
        Ok((first as usize & 0x7f) | ((second as usize) << 7))
    } else {
        Ok(first as usize)
    }
}
