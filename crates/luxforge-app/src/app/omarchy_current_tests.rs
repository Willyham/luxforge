//! Omarchy's current theme read from a home directory laid out as each Omarchy release lays it
//! out: Omarchy 4's name before Omarchy 3's, Omarchy 3.0's link, and nothing for a name that is
//! not a folder's.
use super::omarchy_current::{current_slug, home};
use luxforge_core::theme::MAX_THEME_FOLDER;
use std::{fs, path::Path};

const OMARCHY_4: &str = ".local/state/omarchy/current/theme.name";
const OMARCHY_3: &str = ".config/omarchy/current/theme.name";

fn write(home: &Path, file: &str, text: &str) {
    let path = home.join(file);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

#[test]
fn omarchys_current_theme_is_read_from_each_releases_layout() {
    let root = luxforge_testbase::paths::temp_path("omarchy-current");
    fs::create_dir_all(&root).unwrap();
    assert_eq!(current_slug(&root), None, "no Omarchy");

    // Omarchy 3.0 to 3.2: a link to the theme's folder.
    #[cfg(unix)]
    {
        let folder = root.join(".local/share/omarchy/themes/rose-pine");
        fs::create_dir_all(&folder).unwrap();
        let link = root.join(".config/omarchy/current/theme");
        fs::create_dir_all(link.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&folder, &link).unwrap();
        assert_eq!(current_slug(&root).as_deref(), Some("rose-pine"));
    }
    // Omarchy 3.3 to 3.8 name it, which wins over the link.
    write(&root, OMARCHY_3, "everforest\n");
    assert_eq!(current_slug(&root).as_deref(), Some("everforest"));
    // Omarchy 4's name wins over Omarchy 3's.
    write(&root, OMARCHY_4, "tokyo-night\n");
    assert_eq!(current_slug(&root).as_deref(), Some("tokyo-night"));

    // A name that is not a folder's is no name, and the next layout is tried.
    for text in ["", "  \n", "../nord", ".hidden", "a/b", "nord\u{7}"] {
        write(&root, OMARCHY_4, text);
        assert_eq!(
            current_slug(&root).as_deref(),
            Some("everforest"),
            "{text:?}"
        );
    }
    write(&root, OMARCHY_4, &"a".repeat(MAX_THEME_FOLDER));
    assert_eq!(
        current_slug(&root).map(|slug| slug.len()),
        Some(MAX_THEME_FOLDER)
    );
    write(&root, OMARCHY_4, &"a".repeat(MAX_THEME_FOLDER + 1));
    assert_eq!(current_slug(&root).as_deref(), Some("everforest"));
    // A folder where the name should be is not read.
    fs::remove_file(root.join(OMARCHY_4)).unwrap();
    fs::create_dir(root.join(OMARCHY_4)).unwrap();
    assert_eq!(current_slug(&root).as_deref(), Some("everforest"));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn an_automated_launch_never_looks_for_omarchy() {
    assert_eq!(home(true, false), None);
    assert_eq!(home(false, true), None);
    if !cfg!(target_os = "linux") {
        assert_eq!(home(false, false), None);
    }
}
