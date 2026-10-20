use crate::{common::config::Config, database::Db};

use super::{crypto, store, Clock, Error, SystemClock};
use std::{
  fs::{self, OpenOptions},
  io::Write,
  os::unix::fs::{MetadataExt, OpenOptionsExt},
  path::{Path, PathBuf},
};

struct EnrollmentFile(PathBuf);
impl EnrollmentFile {
  fn create(path: &Path, uri: &str) -> Result<Self, Error> {
    let parent = path.parent().ok_or(Error::Unavailable)?;
    let metadata = fs::symlink_metadata(parent).map_err(|_| Error::Unavailable)?;
    if !path.is_absolute()
      || !metadata.is_dir()
      || metadata.mode() & 0o7777 != 0o700
      || fs::canonicalize(parent).map_err(|_| Error::Unavailable)? != parent
    {
      return Err(Error::Unavailable);
    }
    let mut file = OpenOptions::new()
      .write(true)
      .create_new(true)
      .mode(0o600)
      .open(path)
      .map_err(|_| Error::Unavailable)?;
    let artifact = Self(path.to_owned());
    if file.metadata().map_err(|_| Error::Unavailable)?.uid() != metadata.uid() {
      return Err(Error::Unavailable);
    }
    file
      .write_all(uri.as_bytes())
      .map_err(|_| Error::Unavailable)?;
    file.sync_all().map_err(|_| Error::Unavailable)?;
    Ok(artifact)
  }
}
impl Drop for EnrollmentFile {
  fn drop(&mut self) {
    if fs::remove_file(&self.0).is_err() {
      eprintln!("Unable to remove private enrollment artifact; remove it manually.");
    }
  }
}

pub async fn bootstrap(db: &Db, cfg: &Config, path: &Path) -> Result<(), Error> {
  if cfg.doctor_totp_key.is_none() {
    return Err(Error::Unavailable);
  }
  if store::doctor(db).await?.is_some() {
    return Err(Error::Exists);
  }
  let password = rpassword::prompt_password("Doctor password (12+ characters, at most 72 bytes): ")
    .map_err(|_| Error::Unavailable)?;
  let confirmation =
    rpassword::prompt_password("Confirm password: ").map_err(|_| Error::Unavailable)?;
  if !crypto::valid_password(&password) || !crypto::equal(&password, &confirmation) {
    return Err(Error::Invalid);
  }
  let secret = crypto::random::<20>()?;
  let uri = crypto::totp(secret)?
    .to_url()
    .map_err(|_| Error::Unavailable)?;
  let _artifact = EnrollmentFile::create(path, &uri)?;
  eprintln!("Import the URI from the private enrollment file into your authenticator. It will be removed after this step.");
  let code =
    rpassword::prompt_password("Current authenticator code: ").map_err(|_| Error::Unavailable)?;
  enroll(db, cfg, password, secret, &code, SystemClock.now()).await
}

pub async fn enroll(
  db: &Db,
  cfg: &Config,
  password: String,
  secret: [u8; 20],
  code: &str,
  now: i64,
) -> Result<(), Error> {
  let key = cfg.doctor_totp_key.as_ref().ok_or(Error::Unavailable)?;
  if store::doctor(db).await?.is_some() {
    return Err(Error::Exists);
  }
  let step = crypto::accepted_step(secret, code, now)?;
  let hash = crypto::password_hash(password, cfg.bcrypt_cost).await?;
  let (ciphertext, nonce) = crypto::encrypt(key, &secret)?;
  store::insert_doctor(db, hash, ciphertext, nonce, step).await
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::{fs, os::unix::fs::PermissionsExt};

  #[test]
  fn enrollment_artifact_is_exclusive_private_and_removed_on_drop() {
    let dir = std::env::temp_dir().join(format!("doctor-enrollment-{}", uuid::Uuid::now_v7()));
    fs::create_dir(&dir).unwrap();
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
    let path = dir.join("enrollment.txt");
    {
      let _artifact = EnrollmentFile::create(&path, "private-test-artifact").unwrap();
      assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
      );
      assert!(EnrollmentFile::create(&path, "must-not-replace").is_err());
      assert_eq!(fs::read_to_string(&path).unwrap(), "private-test-artifact");
    }
    assert!(!path.exists());
    let failed_operation = || -> Result<(), Error> {
      let _artifact = EnrollmentFile::create(&path, "private-test-artifact")?;
      Err(Error::Invalid)
    };
    assert!(failed_operation().is_err());
    assert!(!path.exists(), "Normal failure must clean up the artifact");
    std::os::unix::fs::symlink(dir.join("nonexistent-target"), &path).unwrap();
    assert!(EnrollmentFile::create(&path, "must-not-follow-symlink").is_err());
    assert!(fs::symlink_metadata(&path).unwrap().is_symlink());
    fs::remove_file(&path).unwrap();
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(EnrollmentFile::create(&path, "must-not-create").is_err());
    assert!(!path.exists());
    fs::remove_dir(dir).unwrap();
  }
}
