/// Explicit VM folders and discovered application IDs use separate namespaces.
/// Invalid or empty selections must never fall back to saving every application.
pub(crate) fn parse_backup_selections(
    paths: &[String],
) -> Result<(Vec<String>, Vec<String>), String> {
    if paths.is_empty() {
        return Err("Select an application or folder to save.".into());
    }
    let mut apps = Vec::new();
    let mut folders = Vec::new();
    for path in paths {
        if let Some(folder) = path.strip_prefix("/folders/") {
            if folder.trim().is_empty() {
                return Err("Folder path is empty.".into());
            }
            if !folders.contains(path) {
                folders.push(path.clone());
            }
        } else if path == "/apps" || path == "/apps/*" {
            if !apps.contains(&"*".to_string()) {
                apps.push("*".to_string());
            }
        } else if let Some(id) = path
            .strip_prefix("/apps/")
            .filter(|id| !id.is_empty() && !id.contains('/'))
        {
            if !apps.contains(&id.to_string()) {
                apps.push(id.to_string());
            }
        } else {
            return Err(format!("Unsupported save selection: {path}"));
        }
    }
    // An explicit all-app selection already includes individual app IDs.
    if apps.iter().any(|id| id == "*") {
        apps = vec!["*".into()];
    }
    Ok((apps, folders))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn folders_never_select_all_apps() {
        let path = "/folders/Documents/My Project".to_string();
        let (apps, folders) = parse_backup_selections(&[path.clone(), path.clone()]).unwrap();
        assert!(apps.is_empty());
        assert_eq!(folders, vec![path]);
    }
    #[test]
    fn rejects_empty_and_unrecognized_selections() {
        assert!(parse_backup_selections(&[]).is_err());
        for path in [
            "/home/gamer/project",
            "/folders/",
            "/apps/steam:480/nested",
            "/catalog",
        ] {
            assert!(parse_backup_selections(&[path.into()]).is_err());
        }
    }
    #[test]
    fn supports_mixed_and_explicit_all_app_selections() {
        let (apps, folders) =
            parse_backup_selections(&["/apps/steam:480".into(), "/folders/project".into()])
                .unwrap();
        assert_eq!(apps, vec!["steam:480"]);
        assert_eq!(folders, vec!["/folders/project"]);
        let (apps, _) =
            parse_backup_selections(&["/apps/steam:480".into(), "/apps".into()]).unwrap();
        assert_eq!(apps, vec!["*"]);
    }
}
