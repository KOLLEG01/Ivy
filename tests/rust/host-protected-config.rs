use ivy_contracts::{IvyError, IvyJson};
use ivy_host_runtime::config::protected_json_file;
use std::{fs, path::PathBuf};

struct Directory(PathBuf);
impl Drop for Directory {
    fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); }
}

#[tokio::test]
async fn protected_configuration_uses_native_files_and_bounded_utf8_json() {
    let root = Directory(std::env::temp_dir().join(format!("ivy-protected-config-{}", uuid::Uuid::new_v4())));
    fs::create_dir(&root.0).unwrap();
    let path = root.0.join("service.json");
    let invalid = || IvyError::new("configuration_unprotected", "Invalid service configuration.");
    assert!(protected_json_file(path.clone(), 128, invalid()).await.unwrap().is_none());
    fs::write(&path, br#"{"minimumMessageAgeMinutes":10}"#).unwrap();
    #[cfg(unix)] {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    }
    let value = protected_json_file(path.clone(), 128, invalid()).await.unwrap().unwrap();
    assert_eq!(value.get("minimumMessageAgeMinutes"), Some(&IvyJson::Number(10.0)));
    assert_eq!(protected_json_file(path.clone(), 8, invalid()).await.unwrap_err().code, "configuration_unprotected");
    fs::write(&path, [0xff, 0xfe]).unwrap();
    assert_eq!(protected_json_file(path.clone(), 128, invalid()).await.unwrap_err().code, "configuration_unprotected");
    fs::write(&path, b"{}").unwrap();
    fs::hard_link(&path, root.0.join("linked.json")).unwrap();
    assert_eq!(protected_json_file(path, 128, invalid()).await.unwrap_err().code, "configuration_unprotected");
}
