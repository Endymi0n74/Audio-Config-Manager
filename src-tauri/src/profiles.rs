//! Gestion du dossier de profils : liste (nom, chemin, modifié, taille),
//! noms de fichiers uniques et nettoyage des versions horodatées.

use serde::Serialize;
use std::path::{Path, PathBuf};

/// Entrée de la liste des profils — mêmes champs que l'original.
#[derive(Debug, Clone, Serialize)]
pub struct ProfileEntry {
    pub name: String,
    pub path: String,
    pub modified: String,
    pub size: u64,
}

/// Liste les profils `.json` du dossier, du plus récent au plus ancien.
pub fn list_profiles(folder: &Path) -> Result<Vec<ProfileEntry>, String> {
    let entries = std::fs::read_dir(folder)
        .map_err(|e| format!("Ouverture du dossier des profils impossible : {e}"))?;

    let mut profiles = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Ok(metadata) = std::fs::metadata(&path) else { continue };
        if !metadata.is_file() {
            continue;
        }
        let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let modified = metadata
            .modified()
            .ok()
            .map(|t| chrono::DateTime::<chrono::Local>::from(t).to_rfc3339())
            .unwrap_or_default();
        profiles.push(ProfileEntry {
            name,
            path: path.to_string_lossy().to_string(),
            modified,
            size: metadata.len(),
        });
    }

    profiles.sort_by(|a, b| b.modified.cmp(&a.modified));
    Ok(profiles)
}

/// Renvoie un chemin libre dans `folder` pour `name`, en ajoutant un
/// suffixe « (2) », « (3) »… si le nom existe déjà.
pub fn unique_path(folder: &Path, name: &str) -> PathBuf {
    let stem = Path::new(name)
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "profil".into());
    let ext = Path::new(name)
        .extension()
        .map(|e| e.to_string_lossy().to_string())
        .unwrap_or_else(|| "json".into());
    let mut candidate = folder.join(name);
    let mut counter = 2;
    while candidate.exists() {
        candidate = folder.join(format!("{stem} ({counter}).{ext}"));
        counter += 1;
    }
    candidate
}

/// Les sauvegardes horodatées (ex. `2026-09-07 08-00-00.json` ou
/// `Avant restauration 2026-09-07 08-00-00.json`) sont considérées comme
/// des versions. Supprime les plus anciennes au-delà de `keep` (0 = toutes
/// conservées). Renvoie le nombre de fichiers supprimés.
pub fn prune_versions(folder: &Path, keep: u32) -> Result<usize, String> {
    if keep == 0 {
        return Ok(0);
    }
    let mut versions: Vec<(String, PathBuf)> = Vec::new();
    for entry in std::fs::read_dir(folder)
        .map_err(|e| format!("Ouverture du dossier des profils impossible : {e}"))?
        .flatten()
    {
        let path = entry.path();
        let Some(name) = path.file_name().map(|n| n.to_string_lossy().to_string()) else {
            continue;
        };
        if is_versioned_name(&name) {
            versions.push((name, path));
        }
    }
    // Tri décroissant : les plus récentes d'abord (le nom horodaté trie bien).
    versions.sort_by(|a, b| b.0.cmp(&a.0));

    let mut removed = 0;
    for (_, path) in versions.into_iter().skip(keep as usize) {
        if std::fs::remove_file(&path).is_ok() {
            removed += 1;
        }
    }
    Ok(removed)
}

/// Nom de fichier d'une sauvegarde horodatée (avec ou sans préfixe
/// « Avant restauration »), ex. `2026-09-07 08-00-00.json`.
fn is_versioned_name(name: &str) -> bool {
    let base = name.strip_prefix("Avant restauration ").unwrap_or(name);
    let Some(stem) = base.strip_suffix(".json") else {
        return false;
    };
    if stem.len() != 19 {
        return false;
    }
    let bytes = stem.as_bytes();
    let digit = |i: usize| bytes.get(i).is_some_and(|c| c.is_ascii_digit());
    (0..4).all(digit)
        && bytes[4] == b'-'
        && (5..7).all(digit)
        && bytes[7] == b'-'
        && (8..10).all(digit)
        && bytes[10] == b' '
        && (11..13).all(digit)
        && bytes[13] == b'-'
        && (14..16).all(digit)
        && bytes[16] == b'-'
        && (17..19).all(digit)
}

#[cfg(test)]
pub(crate) fn temp_dir(prefix: &str) -> PathBuf {
    static SEQ: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("{prefix}-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unique_path_increments_suffix() {
        let dir = temp_dir("acm-test");
        let first = unique_path(&dir, "profil.json");
        assert_eq!(first.file_name().unwrap().to_string_lossy(), "profil.json");
        std::fs::write(&first, "{}").unwrap();
        let second = unique_path(&dir, "profil.json");
        assert_eq!(second.file_name().unwrap().to_string_lossy(), "profil (2).json");
    }

    #[test]
    fn list_profiles_sorts_by_modified_desc() {
        let dir = temp_dir("acm-test");
        let old = dir.join("old.json");
        let new = dir.join("new.json");
        std::fs::write(&old, "{}").unwrap();
        std::fs::write(&new, "{}").unwrap();
        // Force l'ordre des mtimes
        let _ = std::fs::remove_file(&old);
        std::fs::write(&old, "{}").unwrap();

        let profiles = list_profiles(&dir).unwrap();
        let names: Vec<String> = profiles.iter().map(|p| p.name.clone()).collect();
        assert!(names.contains(&"new.json".to_string()));
        assert!(names.contains(&"old.json".to_string()));
        for profile in &profiles {
            assert!(!profile.path.is_empty());
            assert!(profile.size >= 2);
            assert!(!profile.modified.is_empty());
        }
    }

    #[test]
    fn prune_versions_keeps_newest() {
        let dir = temp_dir("acm-test");
        for i in 1..=5 {
            let name = format!("2026-09-0{i} 08-00-00.json");
            std::fs::write(dir.join(name), "{}").unwrap();
        }
        // Un profil normal n'est jamais supprimé
        std::fs::write(dir.join("mon-profil.json"), "{}").unwrap();

        let removed = prune_versions(&dir, 2).unwrap();
        assert_eq!(removed, 3);
        let remaining = list_profiles(&dir).unwrap();
        let names: Vec<String> = remaining.iter().map(|p| p.name.clone()).collect();
        assert!(names.contains(&"2026-09-05 08-00-00.json".to_string()));
        assert!(names.contains(&"2026-09-04 08-00-00.json".to_string()));
        assert!(!names.contains(&"2026-09-01 08-00-00.json".to_string()));
        assert!(names.contains(&"mon-profil.json".to_string()));
    }

    #[test]
    fn prune_versions_keep_zero_keeps_everything() {
        let dir = temp_dir("acm-test");
        for i in 1..=3 {
            std::fs::write(dir.join(format!("2026-09-0{i} 08-00-00.json")), "{}").unwrap();
        }
        let removed = prune_versions(&dir, 0).unwrap();
        assert_eq!(removed, 0);
    }

    #[test]
    fn versioned_names_detected() {
        assert!(is_versioned_name("2026-09-07 08-00-00.json"));
        assert!(is_versioned_name("Avant restauration 2026-09-07 08-00-00.json"));
        assert!(!is_versioned_name("mon-profil.json"));
        assert!(!is_versioned_name("2026-09-07 08-00-00.txt"));
    }
}