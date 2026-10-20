use crate::modules::whatsapp::client::MAX_MEDIA_BYTES;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum InvalidMedia {
  Unsupported,
  TooLarge,
}

pub(crate) fn supported_mime(mime: &str) -> Option<&'static str> {
  let mime = mime.split(';').next().unwrap_or_default().trim();
  if mime.eq_ignore_ascii_case("image/jpeg") {
    Some("image/jpeg")
  } else if mime.eq_ignore_ascii_case("image/png") {
    Some("image/png")
  } else if mime.eq_ignore_ascii_case("image/webp") {
    Some("image/webp")
  } else if mime.eq_ignore_ascii_case("application/pdf") {
    Some("application/pdf")
  } else {
    None
  }
}

pub(crate) fn canonical_mime(mime: &str, bytes: &[u8]) -> Result<&'static str, InvalidMedia> {
  if bytes.is_empty() {
    return Err(InvalidMedia::Unsupported);
  }
  if bytes.len() > MAX_MEDIA_BYTES {
    return Err(InvalidMedia::TooLarge);
  }
  let canonical = supported_mime(mime).ok_or(InvalidMedia::Unsupported)?;
  let supported = match canonical {
    "image/jpeg" => bytes.starts_with(&[0xff, 0xd8, 0xff]),
    "image/png" => {
      bytes.len() >= 24
        && bytes.starts_with(b"\x89PNG\r\n\x1a\n")
        && bytes[8..12] == [0, 0, 0, 13]
        && bytes[12..16] == *b"IHDR"
    }
    "image/webp" => {
      bytes.len() >= 12
        && bytes.starts_with(b"RIFF")
        && bytes[8..12] == *b"WEBP"
        && (u64::from(u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]])) + 8)
          <= bytes.len() as u64
    }
    "application/pdf" => bytes.starts_with(b"%PDF-"),
    _ => false,
  };
  if !supported {
    return Err(InvalidMedia::Unsupported);
  }
  Ok(canonical)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn accepts_only_matching_supported_signatures_and_boundaries() {
    let jpeg = [0xff, 0xd8, 0xff, 0];
    assert_eq!(canonical_mime("IMAGE/JPEG", &jpeg), Ok("image/jpeg"));
    let png = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR\0\0\0\0\0\0\0\0";
    let webp = b"RIFF\x04\0\0\0WEBP";
    let pdf = b"%PDF-1.7";
    assert_eq!(canonical_mime("image/jpeg", &jpeg), Ok("image/jpeg"));
    assert_eq!(canonical_mime("image/png", png), Ok("image/png"));
    assert_eq!(canonical_mime("image/webp", webp), Ok("image/webp"));
    assert_eq!(
      canonical_mime("application/pdf", pdf),
      Ok("application/pdf")
    );
    assert_eq!(
      canonical_mime("image/png", &jpeg),
      Err(InvalidMedia::Unsupported)
    );
    assert_eq!(
      canonical_mime("image/gif", b"GIF89a"),
      Err(InvalidMedia::Unsupported)
    );
    assert_eq!(
      canonical_mime("application/pdf", &vec![b'x'; MAX_MEDIA_BYTES + 1]),
      Err(InvalidMedia::TooLarge)
    );
  }
}
