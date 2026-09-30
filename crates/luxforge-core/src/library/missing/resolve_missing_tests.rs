//! The parts of resolving missing originals that need no owner: the search's walk (what it lists,
//! skips and refuses), how a photograph's result is settled from what was found and who names it,
//! and the bounded memory of what finds verified. Every file is made in a scratch directory.
use super::{
    Looked, MAX_REMEMBERED_FILES, SameBytes, Sought, Verifications, VerifiedFile,
    search::{SearchLimits, device_of, name_key, walk},
    settle,
};
use crate::{
    AssetId, EditorService, ErrorKind,
    catalog_types::{FindResult, Volume, VolumeId},
    jobs::JobControl,
};
use luxforge_testbase::paths::temp_dir;
use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

fn write(path: &Path, bytes: &[u8]) -> PathBuf {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
    path.to_path_buf()
}

fn names(names: &[&str]) -> HashSet<String> {
    names.iter().map(|name| name_key(name)).collect()
}

/// Walk `root` for `wanted` with `limits`, answering each name's files as paths.
fn listed(
    root: &Path,
    wanted: &[&str],
    limits: SearchLimits,
) -> Result<Vec<(String, Vec<PathBuf>)>, crate::Error> {
    let device = device_of(&root.symlink_metadata().unwrap());
    let listed = walk(root, device, &names(wanted), limits, &JobControl::new())?;
    let mut listed: Vec<_> = listed
        .into_iter()
        .map(|(name, files)| (name, files.into_iter().map(|file| file.path).collect()))
        .collect();
    listed.sort();
    Ok(listed)
}

/// The walk lists every file of a sought name in the chosen folder and its subfolders, in path
/// order, ignoring case where the platform does, with its length; it never enters a hidden folder,
/// a package, another application's cache or a folder deeper than its limit, and never follows a
/// symbolic link, to a folder or to a file, out of the folder.
#[test]
fn resolve_missing_walk_lists_same_name_files_and_skips_what_it_must() {
    let dir = temp_dir("resolve-missing-walk").canonicalize().unwrap();
    let root = dir.join("search");
    let outside = write(&dir.join("outside").join("DSC_0001.JPG"), b"outside");
    let found = [
        write(&root.join("a").join("DSC_0001.JPG"), b"one"),
        write(&root.join("b").join("c").join("DSC_0001.JPG"), b"three"),
        write(&root.join("DSC_0001.JPG"), b"root"),
    ];
    let other_case = write(&root.join("d").join("dsc_0001.jpg"), b"lower");
    write(&root.join("a").join("other.jpg"), b"not sought");
    for skipped in [
        ".hidden",
        "Photos Library.photoslibrary",
        "Lightroom Catalog Previews.lrdata",
        "Tool.app",
        "CaptureOne",
    ] {
        write(&root.join(skipped).join("DSC_0001.JPG"), b"skipped");
    }
    // Four levels below the root: past a depth limit of three.
    write(
        &root
            .join("1")
            .join("2")
            .join("3")
            .join("4")
            .join("DSC_0001.JPG"),
        b"deep",
    );
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(dir.join("outside"), root.join("linked-folder")).unwrap();
        fs::create_dir_all(root.join("e")).unwrap();
        std::os::unix::fs::symlink(&outside, root.join("e").join("DSC_0001.JPG")).unwrap();
    }
    let limits = SearchLimits {
        depth: 3,
        ..SearchLimits::default()
    };
    let mut expected: Vec<PathBuf> = found.to_vec();
    if cfg!(any(target_os = "macos", windows)) {
        expected.push(other_case);
    }
    expected.sort();
    assert_eq!(
        listed(&root, &["DSC_0001.JPG"], limits).unwrap(),
        [(name_key("DSC_0001.JPG"), expected)]
    );
    assert!(listed(&root, &["missing.jpg"], limits).unwrap().is_empty());
    assert!(outside.exists(), "nothing outside was touched");
    fs::remove_dir_all(dir).unwrap();
}

/// A walk past its file or folder limit is refused with `resource-limit`, a cancelled one stops
/// with `cancelled`, and a subfolder that cannot be read fails it with `read-error` rather than
/// reading as holding nothing.
#[test]
fn resolve_missing_walk_is_bounded_cancellable_and_honest_about_unreadable_folders() {
    let dir = temp_dir("resolve-missing-bounds").canonicalize().unwrap();
    for folder in ["a", "b", "c"] {
        write(&dir.join(folder).join("x.jpg"), b"x");
        write(&dir.join(folder).join("y.jpg"), b"y");
    }
    let limited = |limits| listed(&dir, &["x.jpg"], limits).map(|_| ()).unwrap_err();
    let files = limited(SearchLimits {
        files: 5,
        ..SearchLimits::default()
    });
    assert_eq!(files.kind, ErrorKind::ResourceLimit, "{files:?}");
    assert!(
        files.detail.contains("more than 5 files"),
        "{}",
        files.detail
    );
    let folders = limited(SearchLimits {
        folders: 3,
        ..SearchLimits::default()
    });
    assert_eq!(folders.kind, ErrorKind::ResourceLimit, "{folders:?}");
    assert_eq!(
        listed(&dir, &["x.jpg"], SearchLimits::default())
            .unwrap()
            .into_iter()
            .map(|(_, files)| files.len())
            .collect::<Vec<_>>(),
        [3]
    );

    let control = JobControl::new();
    control.cancel("stopped");
    let device = device_of(&dir.symlink_metadata().unwrap());
    let cancelled = walk(
        &dir,
        device,
        &names(&["x.jpg"]),
        SearchLimits::default(),
        &control,
    )
    .unwrap_err();
    assert_eq!(cancelled.kind, ErrorKind::Cancelled);

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let locked = dir.join("b");
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
        // A process that may read anything (root) cannot be refused.
        if fs::read_dir(&locked).is_err() {
            let unreadable = listed(&dir, &["x.jpg"], SearchLimits::default()).unwrap_err();
            assert_eq!(unreadable.kind, ErrorKind::FileAccess, "{unreadable:?}");
            assert!(
                unreadable.detail.contains(&locked.display().to_string()),
                "{}",
                unreadable.detail
            );
        }
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
    }
    fs::remove_dir_all(dir).unwrap();
}

fn same(path: &Path) -> SameBytes {
    SameBytes {
        path: path.to_path_buf(),
        signature: EditorService::request_signature(path).unwrap().1,
    }
}

/// Each photograph's result follows from what was found and who names each file: one free
/// same-bytes file is found (its own current file counting as free), several are to choose
/// between, one another photograph names and none free is claimed and never offered, and
/// otherwise a same-name file of other bytes, or nothing.
#[test]
fn resolve_missing_settles_each_result_from_what_was_found_and_who_names_it() {
    let dir = temp_dir("resolve-missing-settle").canonicalize().unwrap();
    let a = write(&dir.join("a").join("DSC_0001.JPG"), b"same");
    let b = write(&dir.join("b").join("DSC_0001.JPG"), b"same");
    let c = write(&dir.join("c").join("DSC_0001.JPG"), b"other");
    let sought = Sought {
        asset_id: AssetId::new(),
        file_name: "DSC_0001.JPG".into(),
        byte_len: 4,
        fingerprint: String::new(),
    };
    let other = AssetId::new();
    let looked = |same_bytes: &[&Path], different: Option<&Path>| Looked {
        same: same_bytes.iter().map(|path| same(path)).collect(),
        different: different.map(Path::to_path_buf),
    };
    let result = |looked: &Looked, claims: &[Vec<AssetId>]| {
        let (result, free) = settle(&sought, looked, claims);
        (
            result,
            free.iter()
                .map(|file| file.path.clone())
                .collect::<Vec<_>>(),
        )
    };

    let one = looked(&[&a], Some(&c));
    assert_eq!(
        result(&one, &[vec![]]),
        (FindResult::Found { path: a.clone() }, vec![a.clone()])
    );
    assert_eq!(
        result(&one, &[vec![sought.asset_id.clone()]]),
        (FindResult::Found { path: a.clone() }, vec![a.clone()]),
        "its own current file is its to keep"
    );
    assert_eq!(
        result(&one, &[vec![other.clone()]]),
        (
            FindResult::Claimed {
                path: a.clone(),
                by: other.clone()
            },
            vec![]
        ),
        "a file another photograph names is never offered, even with a different file beside it"
    );
    assert_eq!(
        result(&one, &[vec![sought.asset_id.clone(), other.clone()]],).0,
        FindResult::Claimed {
            path: a.clone(),
            by: other.clone()
        },
        "named by path by one and by identity by another"
    );

    let two = looked(&[&a, &b], None);
    assert_eq!(
        result(&two, &[vec![], vec![]]),
        (
            FindResult::SeveralIdentical {
                paths: vec![a.clone(), b.clone()]
            },
            vec![a.clone(), b.clone()]
        )
    );
    assert_eq!(
        result(&two, &[vec![other.clone()], vec![]]),
        (FindResult::Found { path: b.clone() }, vec![b.clone()]),
        "the claimed copy is left out and the free one found"
    );

    assert_eq!(
        result(&looked(&[], Some(&c)), &[]).0,
        FindResult::DifferentBytes { path: c.clone() }
    );
    assert_eq!(result(&looked(&[], None), &[]).0, FindResult::NotFound);
    fs::remove_dir_all(dir).unwrap();
}

/// What finds verified is remembered per photograph, replaced by the photograph's next result,
/// forgotten file by file, and bounded: past the bound the photograph remembered longest ago goes
/// first.
#[test]
fn resolve_missing_remembers_verified_files_bounded_and_replaced() {
    let dir = temp_dir("resolve-missing-remember").canonicalize().unwrap();
    let file = write(&dir.join("DSC_0001.JPG"), b"bytes");
    let other = write(&dir.join("DSC_0002.JPG"), b"bytes");
    let volume = Arc::new(Volume {
        id: VolumeId::new(),
        mount_point: "/".into(),
        label: "Test".into(),
        removable: false,
        platform_id: None,
        last_seen_ms: 1,
    });
    let verified = |path: &Path| VerifiedFile {
        path: path.to_path_buf(),
        signature: EditorService::request_signature(path).unwrap().1,
        volume: volume.clone(),
    };
    let mut memory = Verifications::default();
    let (first, second) = (AssetId::new(), AssetId::new());
    memory.remember(first.clone(), vec![verified(&file), verified(&other)]);
    assert!(memory.get(&first, &file).is_some());
    assert!(memory.get(&second, &file).is_none(), "per photograph");
    memory.forget(&first, &file);
    assert!(memory.get(&first, &file).is_none());
    assert!(memory.get(&first, &other).is_some());
    memory.remember(first.clone(), vec![]);
    assert!(
        memory.get(&first, &other).is_none(),
        "a newer result replaces an older one, whatever it found"
    );

    memory.remember(first.clone(), vec![verified(&file)]);
    memory.remember(second.clone(), vec![verified(&other)]);
    let many: Vec<AssetId> = (0..MAX_REMEMBERED_FILES - 1)
        .map(|_| AssetId::new())
        .collect();
    let one = verified(&file);
    for asset in &many {
        memory.remember(asset.clone(), vec![one.clone()]);
    }
    assert!(
        memory.get(&first, &file).is_none(),
        "the photograph remembered longest ago is forgotten first"
    );
    assert!(memory.get(&second, &other).is_some());
    assert!(memory.get(many.last().unwrap(), &file).is_some());
    fs::remove_dir_all(dir).unwrap();
}
