use std::{fmt::Display, path::PathBuf};

use tokio::{fs::OpenOptions, io::AsyncWriteExt};

pub struct TomlFileProxy<T> {
    path: PathBuf,
    cache: Option<T>,
}

impl<T> TomlFileProxy<T>
where
    T: serde::de::DeserializeOwned + serde::Serialize,
{
    /// Create a new proxy to the toml file.
    pub async fn new(path: &str) -> Result<Self, Error> {
        let path = PathBuf::from(path);
        OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .await?;
        let path = tokio::fs::canonicalize(path).await?;

        Ok(Self { path, cache: None })
    }

    /// Load data from the file to cache, and returns the cached data
    pub async fn load(&mut self) -> Result<&T, Error> {
        let toml = tokio::fs::read_to_string(&self.path).await?;

        self.cache = toml::from_str::<T>(&toml)?.into();

        Ok(self.cache.as_ref().unwrap())
    }

    /// Save the cached data to the file
    pub async fn save(&mut self) -> Result<(), Error> {
        let cache = match &self.cache {
            Some(c) => c,
            None => return Err(Error::CacheEmpty),
        };

        let toml = toml::to_string_pretty(cache)?;
        let permissions = tokio::fs::metadata(&self.path).await?.permissions();
        let temporary_path = self.path.with_file_name(format!(
            ".{}.{}.tmp",
            self.path.file_name().unwrap_or_default().to_string_lossy(),
            uuid::Uuid::new_v4()
        ));

        let write_result = async {
            let mut temporary_file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary_path)
                .await?;
            tokio::fs::set_permissions(&temporary_path, permissions).await?;
            temporary_file.write_all(toml.as_bytes()).await?;
            temporary_file.sync_all().await?;
            drop(temporary_file);
            tokio::fs::rename(&temporary_path, &self.path).await
        }
        .await;

        if let Err(error) = write_result {
            let _ = tokio::fs::remove_file(&temporary_path).await;
            return Err(error.into());
        }

        Ok(())
    }

    pub fn get_cache(&self) -> Option<&T> {
        self.cache.as_ref()
    }

    pub fn get_cache_mut(&mut self) -> Option<&mut T> {
        self.cache.as_mut()
    }

    pub fn update_cache(&mut self, data: T) {
        self.cache = data.into();
    }

    pub async fn get_cache_or_load(&mut self) -> Result<&T, Error> {
        if let Some(cache) = self.cache.as_ref() {
            return Ok(cache);
        }
        self.load().await
    }

    pub async fn save_with_data(&mut self, data: T) -> Result<(), Error> {
        self.cache = data.into();
        self.save().await
    }
}

#[derive(Debug)]
pub enum Error {
    IoError(std::io::Error),
    TomlError(toml::de::Error),
    TomlSerializeError(toml::ser::Error),
    CacheEmpty,
}
impl Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::IoError(e) => f.write_fmt(format_args!("IO error: {e}")),
            Error::TomlError(e) => f.write_fmt(format_args!("Toml error: {e}")),
            Error::TomlSerializeError(e) => {
                f.write_fmt(format_args!("Toml serialization error: {e}"))
            }
            Error::CacheEmpty => f.write_fmt(format_args!("Cache is empty.")),
        }
    }
}
impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::IoError(error) => Some(error),
            Self::TomlError(error) => Some(error),
            Self::TomlSerializeError(error) => Some(error),
            Self::CacheEmpty => None,
        }
    }
}
impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::IoError(e)
    }
}
impl From<toml::de::Error> for Error {
    fn from(e: toml::de::Error) -> Self {
        Error::TomlError(e)
    }
}
impl From<toml::ser::Error> for Error {
    fn from(e: toml::ser::Error) -> Self {
        Error::TomlSerializeError(e)
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, path::PathBuf};

    use super::{Error, TomlFileProxy};

    #[test]
    fn proxy_errors_expose_their_underlying_cause() {
        let io_error = Error::IoError(std::io::Error::other("disk unavailable"));

        assert_eq!(
            std::error::Error::source(&io_error).unwrap().to_string(),
            "disk unavailable"
        );
        assert!(std::error::Error::source(&Error::CacheEmpty).is_none());
    }

    struct FailsToSerialize;

    impl serde::Serialize for FailsToSerialize {
        fn serialize<S>(&self, _serializer: S) -> Result<S::Ok, S::Error>
        where
            S: serde::Serializer,
        {
            Err(serde::ser::Error::custom(
                "intentional serialization failure",
            ))
        }
    }

    impl<'de> serde::Deserialize<'de> for FailsToSerialize {
        fn deserialize<D>(_deserializer: D) -> Result<Self, D::Error>
        where
            D: serde::Deserializer<'de>,
        {
            Ok(Self)
        }
    }

    fn temp_toml_path() -> PathBuf {
        std::env::temp_dir().join(format!(
            "patrol-toml-file-proxy-{}.toml",
            uuid::Uuid::new_v4()
        ))
    }

    #[tokio::test]
    async fn save_without_cache_returns_cache_empty() {
        let path = temp_toml_path();
        let mut proxy = TomlFileProxy::<HashMap<String, String>>::new(path.to_str().unwrap())
            .await
            .unwrap();

        assert!(matches!(proxy.save().await, Err(Error::CacheEmpty)));

        drop(proxy);
        std::fs::remove_file(path).unwrap();
    }

    #[tokio::test]
    async fn opening_existing_file_preserves_its_contents() {
        let path = temp_toml_path();
        let original = "existing = \"value\"\n";
        std::fs::write(&path, original).unwrap();

        let proxy = TomlFileProxy::<HashMap<String, String>>::new(path.to_str().unwrap())
            .await
            .unwrap();
        drop(proxy);

        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
        std::fs::remove_file(path).unwrap();
    }

    #[tokio::test]
    async fn serialization_failure_is_returned_as_an_error() {
        let path = temp_toml_path();
        let previous_content = "previous = \"data\"\n";
        std::fs::write(&path, previous_content).unwrap();
        let mut proxy = TomlFileProxy::<FailsToSerialize>::new(path.to_str().unwrap())
            .await
            .unwrap();
        proxy.update_cache(FailsToSerialize);

        let result = proxy.save().await;

        assert!(matches!(result, Err(Error::TomlSerializeError(_))));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), previous_content);
        drop(proxy);
        std::fs::remove_file(path).unwrap();
    }

    #[tokio::test]
    async fn failed_atomic_replace_removes_temporary_file() {
        let path = temp_toml_path();
        let mut proxy = TomlFileProxy::<HashMap<String, String>>::new(path.to_str().unwrap())
            .await
            .unwrap();
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        proxy.update_cache(HashMap::from([("page".to_owned(), "content".to_owned())]));

        let result = proxy.save().await;

        assert!(matches!(result, Err(Error::IoError(_))));
        let temporary_prefix = format!(".{}.", path.file_name().unwrap().to_string_lossy());
        let temporary_files = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                name.starts_with(&temporary_prefix) && name.ends_with(".tmp")
            })
            .count();
        assert_eq!(temporary_files, 0);

        drop(proxy);
        std::fs::remove_dir(path).unwrap();
    }

    #[tokio::test]
    async fn saves_cache_and_loads_it_again() {
        let path = temp_toml_path();
        let path_string = path.to_str().unwrap();
        let mut proxy = TomlFileProxy::<HashMap<String, String>>::new(path_string)
            .await
            .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = std::fs::metadata(&path).unwrap().permissions();
            permissions.set_mode(0o640);
            std::fs::set_permissions(&path, permissions).unwrap();
        }
        let mut expected = HashMap::from([
            ("page".to_owned(), "a long value to be replaced".to_owned()),
            ("other".to_owned(), "kept".to_owned()),
        ]);

        proxy.save_with_data(expected.clone()).await.unwrap();
        proxy
            .get_cache_mut()
            .unwrap()
            .insert("page".to_owned(), "short".to_owned());
        expected.insert("page".to_owned(), "short".to_owned());
        proxy.save().await.unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o640);
        }
        drop(proxy);

        let mut reloaded = TomlFileProxy::<HashMap<String, String>>::new(path_string)
            .await
            .unwrap();
        assert_eq!(reloaded.load().await.unwrap(), &expected);

        drop(reloaded);
        std::fs::remove_file(path).unwrap();
    }
}
