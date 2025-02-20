pub mod serialize;

use std::path::{Path, PathBuf};

pub fn absolute_path(base: impl AsRef<Path>, path: impl AsRef<Path>) -> PathBuf {
    let path = path.as_ref();
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.as_ref().join(path)
    }
}
