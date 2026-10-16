//! The Linux release payload's own parts (`docs/design/release-v0.1.md#deliverables`): the
//! user-level desktop integration (`desktop-integration.sh`, the desktop-entry template and the
//! icon in the hicolor layout), the inspection of what the executables import, and the packaged
//! Linux help, `INSTALL.txt`. Everything is copied into the payload, so an extracted payload never
//! reads the checkout.
use crate::*;
use xtask_cli::release_tag::ReleaseTag;

/// The install and uninstall helper, at the top of the payload.
pub const SCRIPT: &str = "desktop-integration.sh";
const SCRIPT_SOURCE: &str = "packaging/linux/desktop-integration.sh";
const TEMPLATE_SOURCE: &str = "packaging/linux/luxforge.desktop.in";
/// The payload's folder of desktop files: the template and `icons/hicolor/…`.
pub const DESKTOP_DIR: &str = "desktop";
pub const TEMPLATE: &str = "luxforge.desktop.in";
/// The application icon in each size the payload carries, as `(file in` [`icon::DIRECTORY`]`,
/// hicolor path)`. `desktop-integration.sh` installs and removes exactly these hicolor paths.
pub const ICONS: &[(&str, &str)] = &[
    ("luxforge-256.png", "256x256/apps/luxforge.png"),
    ("luxforge-512.png", "512x512/apps/luxforge.png"),
    (icon::SOURCE, "scalable/apps/luxforge.svg"),
];
/// The packaged Linux help.
pub const HELP: &str = "INSTALL.txt";

/// The checkout files the Linux payload copies: the helper, the template and every icon.
pub fn inputs(root: &Path) -> Vec<PathBuf> {
    let mut inputs = vec![root.join(SCRIPT_SOURCE), root.join(TEMPLATE_SOURCE)];
    inputs.extend(
        ICONS
            .iter()
            .map(|(file, _)| root.join(icon::DIRECTORY).join(file)),
    );
    inputs
}

/// Adds the payload's Linux parts to `payload`, the extracted `Luxforge/` folder holding the
/// executables, and records what they import in `metadata` as `elf_imports`.
pub fn complete(
    root: &Path,
    payload: &Path,
    tag: &ReleaseTag,
    number: &str,
    executables: &[(&str, &Path)],
    metadata: &mut Value,
) -> Result {
    desktop_integration(root, payload, &tag.version, number)?;
    let imports = xtask_cli::release_elf::inspect(executables)?;
    fs::write(payload.join(HELP), help(&tag.version, &imports)?)?;
    metadata["elf_imports"] = imports;
    Ok(())
}

/// Copies the helper (mode 0755), the template with this build's version and build number, and
/// every icon into `payload`. A missing input, or a payload that already has any of them, is
/// refused.
pub fn desktop_integration(root: &Path, payload: &Path, version: &str, number: &str) -> Result {
    ensure(
        payload.is_dir(),
        format!("Missing Linux payload: {}", payload.display()),
    )?;
    ensure(
        !version.is_empty()
            && version
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b".-+".contains(&b))
            && !number.is_empty()
            && number.bytes().all(|b| b.is_ascii_digit()),
        "A desktop entry's version and build number are plain",
    )?;
    let script = payload.join(SCRIPT);
    let desktop = payload.join(DESKTOP_DIR);
    for existing in [&script, &desktop] {
        ensure(
            existing.symlink_metadata().is_err(),
            format!("The payload already has {}", existing.display()),
        )?;
    }
    for input in inputs(root) {
        ensure(
            input.is_file(),
            format!("Missing Linux payload input: {}", input.display()),
        )?;
    }
    let template = fs::read_to_string(root.join(TEMPLATE_SOURCE))?;
    for placeholder in [
        "Exec=@EXEC@",
        "TryExec=@TRYEXEC@",
        "X-Luxforge-Version=@VERSION@",
        "X-Luxforge-Build=@BUILD@",
    ] {
        ensure(
            template.lines().filter(|line| *line == placeholder).count() == 1,
            format!("{TEMPLATE_SOURCE} has no single {placeholder} line"),
        )?;
    }
    fs::create_dir(&desktop)?;
    fs::write(
        desktop.join(TEMPLATE),
        template
            .replace("@VERSION@", version)
            .replace("@BUILD@", number),
    )?;
    for (file, hicolor) in ICONS {
        let destination = desktop.join("icons/hicolor").join(hicolor);
        fs::create_dir_all(destination.parent().ok_or("Icon folder")?)?;
        fs::copy(root.join(icon::DIRECTORY).join(file), &destination)?;
    }
    fs::copy(root.join(SCRIPT_SOURCE), &script)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for path in files(&desktop)? {
            fs::set_permissions(path, fs::Permissions::from_mode(0o644))?;
        }
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755))?;
    }
    Ok(())
}

/// The Ubuntu package that provides `library`, one of [`xtask_cli::release_elf::SYSTEM_LIBRARIES`].
fn package_of(library: &str) -> &'static str {
    match library {
        "libgcc_s.so.1" => "libgcc-s1",
        "libstdc++.so.6" => "libstdc++6",
        _ => "libc6",
    }
}

/// The packaged Linux help: running, desktop integration, runtime requirements (the linked
/// libraries from `imports`, this build's `elf_imports`) and where Luxforge keeps its data.
pub fn help(version: &str, imports: &Value) -> Result<String> {
    let mut linked = std::collections::BTreeSet::new();
    for entry in imports["executables"]
        .as_object()
        .ok_or("ELF imports name no executables")?
        .values()
    {
        for library in entry["needed"].as_array().ok_or("ELF imports")? {
            linked.insert(library.as_str().ok_or("ELF imports")?.to_owned());
        }
    }
    let packages: std::collections::BTreeSet<_> =
        linked.iter().map(|library| package_of(library)).collect();
    let linked = linked.into_iter().collect::<Vec<_>>().join(", ");
    let packages = packages.into_iter().collect::<Vec<_>>().join(", ");
    let max = imports["max_glibc"].as_str().ok_or("ELF imports")?;
    let baseline = imports["glibc_baseline"].as_str().ok_or("ELF imports")?;
    Ok(format!(
        "Luxforge {version} for x86-64 Linux
=================================

This is a build candidate, not a qualified public release: native Linux GPU, Wayland, X11 and
installation qualification remain open.

Running Luxforge
----------------
Extract the archive anywhere you can write, such as your home folder:

    tar -xzf luxforge-{version}-linux-x86_64.tar.gz

then start the editor with ./Luxforge/luxforge. Nothing needs installing, no root is needed and the
folder finds its own resources wherever it is. The command lines ./Luxforge/luxforge-json and
./Luxforge/luxforge-ctl are beside it. Nothing is added to PATH: run them by path, or link them
into a folder on your PATH yourself.

When you can write the Luxforge folder and the folder it is in, Luxforge updates itself in place,
keeping your photographs, catalogs, settings and logs.

Adding Luxforge to your application launcher (optional)
-------------------------------------------------------
    ./Luxforge/{SCRIPT} install

adds the launcher entry, for you only, as luxforge.desktop in
${{XDG_DATA_HOME:-~/.local/share}}/applications, and the Luxforge icon in
${{XDG_DATA_HOME:-~/.local/share}}/icons/hicolor. The entry starts this folder's luxforge by its
full path, so run install again after moving the folder. An update Luxforge installs itself keeps
the entry current.

    ./Luxforge/{SCRIPT} uninstall

removes the entry and icon install wrote, and nothing else. Neither command needs root, edits PATH
or a shell profile, or installs a driver. To remove Luxforge, run uninstall, then delete the
Luxforge folder.

Requirements
------------
Luxforge is built on Ubuntu 24.04 LTS for x86-64, its baseline. Other distributions may work but
are not tested.

* A Vulkan graphics driver. Luxforge loads the Vulkan loader, libvulkan.so.1 (libvulkan1), and
  draws with the driver for your graphics hardware: Mesa's (mesa-vulkan-drivers) or your vendor's.
  Luxforge ships no graphics driver. Without a Vulkan adapter the editor cannot open its window;
  luxforge-json still works.
* A Wayland or X11 desktop session. Luxforge loads libxkbcommon.so.0 (libxkbcommon0), and
  - on Wayland, libwayland-client.so.0 (libwayland-client0);
  - on X11, libX11.so.6, libX11-xcb.so.1, libXcursor.so.1, libXi.so.6, libxcb.so.1 and
    libxkbcommon-x11.so.0 (libx11-6, libx11-xcb1, libxcursor1, libxi6, libxcb1,
    libxkbcommon-x11-0).
* The libraries the executables link, needing glibc {max} at most (the baseline has {baseline}):
  {linked}, from {packages}. build.json records each executable's imports under elf_imports.
* For opening and choosing files, the XDG desktop portal (xdg-desktop-portal) with a file-chooser
  backend such as xdg-desktop-portal-gnome, xdg-desktop-portal-gtk or xdg-desktop-portal-kde, on a
  D-Bus session. Without them the file chooser cannot open.
* Stored secrets are not supported on Linux yet: a module that needs one reports that it is not
  ready, and nothing is stored in plain text.

Your photographs and data
-------------------------
Luxforge never writes to your original photographs. It keeps:

    settings and the default catalog   ${{XDG_CONFIG_HOME:-~/.config}}/luxforge (catalog.sqlite)
    each catalog's own files           beside the catalog: CATALOG.artifacts/, its index and log
    downloaded resources               ${{XDG_DATA_HOME:-~/.local/share}}/luxforge
    logs                               ${{XDG_STATE_HOME:-~/.local/state}}/luxforge/logs

Updating Luxforge, either {SCRIPT} command and deleting the Luxforge folder leave all of
them, and your photographs, as they are. Back up a catalog with its CATALOG.artifacts/ folder while Luxforge is
closed. A catalog in a format this version does not support is refused, never changed.
"
    ))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use luxforge_test_macros::slow;
    use std::os::unix::fs::PermissionsExt;

    /// The repository this test was built from.
    fn repository() -> &'static Path {
        Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap()
    }

    /// A checkout holding the committed Linux packaging and, with `icons`, the committed icons.
    fn checkout(icons: bool) -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("packaging/linux")).unwrap();
        fs::create_dir_all(root.path().join(icon::DIRECTORY)).unwrap();
        for source in [SCRIPT_SOURCE, TEMPLATE_SOURCE] {
            fs::copy(repository().join(source), root.path().join(source)).unwrap();
        }
        if icons {
            for (file, _) in ICONS {
                let relative = Path::new(icon::DIRECTORY).join(file);
                fs::copy(repository().join(&relative), root.path().join(relative)).unwrap();
            }
        }
        root
    }

    fn committed_icon() -> Vec<u8> {
        fs::read(repository().join(icon::DIRECTORY).join("luxforge-256.png")).unwrap()
    }

    /// An extracted payload at `dir`: an executable editor stand-in, `build.json` and the
    /// desktop integration.
    fn payload(root: &Path, dir: &Path) {
        fs::create_dir_all(dir).unwrap();
        fs::write(dir.join("luxforge"), "#!/bin/sh\n").unwrap();
        fs::set_permissions(dir.join("luxforge"), fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(
            dir.join("build.json"),
            r#"{"version":"0.1.0","build_number":"7","target":"x86_64-unknown-linux-gnu"}"#,
        )
        .unwrap();
        desktop_integration(root, dir, "0.1.0", "7").unwrap();
    }

    fn mode(path: &Path) -> u32 {
        fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    /// Runs the payload's helper with `data` as `XDG_DATA_HOME`, from an unrelated directory.
    fn run(payload: &Path, command: &str, data: &Path, cwd: &Path) -> std::process::Output {
        Command::new("sh")
            .arg(payload.join(SCRIPT))
            .arg(command)
            .current_dir(cwd)
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap())
            .env("HOME", cwd)
            .env("XDG_DATA_HOME", data)
            .output()
            .unwrap()
    }

    /// The program an `Exec` value runs, read as the Desktop Entry Specification says: the string
    /// escapes first, then the quoting rule, then `%%`.
    fn exec_program(value: &str) -> String {
        let mut unescaped = String::new();
        let mut chars = value.chars();
        while let Some(c) = chars.next() {
            if c == '\\' {
                match chars.next().unwrap() {
                    's' => unescaped.push(' '),
                    'n' => unescaped.push('\n'),
                    't' => unescaped.push('\t'),
                    'r' => unescaped.push('\r'),
                    '\\' => unescaped.push('\\'),
                    other => panic!("invalid string escape \\{other} in {value}"),
                }
            } else {
                unescaped.push(c);
            }
        }
        let quoted = unescaped.strip_prefix('"').expect("a quoted program");
        let mut program = String::new();
        let mut chars = quoted.chars();
        loop {
            match chars.next().expect("a closed quote") {
                '"' => break,
                '\\' => {
                    let c = chars.next().unwrap();
                    assert!(matches!(c, '"' | '`' | '$' | '\\'), "escaped {c}");
                    program.push(c);
                }
                c => {
                    assert!(!matches!(c, '`' | '$'), "unescaped {c} in {value}");
                    program.push(c);
                }
            }
        }
        assert_eq!(chars.as_str(), "", "the entry passes no arguments");
        assert!(!program.replace("%%", "").contains('%'), "a field code");
        program.replace("%%", "%")
    }

    fn key<'a>(entry: &'a str, key: &str) -> &'a str {
        entry
            .lines()
            .find_map(|line| line.strip_prefix(key)?.strip_prefix('='))
            .unwrap_or_else(|| panic!("no {key} in {entry}"))
    }

    #[test]
    fn the_payload_carries_the_helper_template_and_icons_with_their_modes() {
        let root = checkout(true);
        let out = tempfile::tempdir().unwrap();
        let dir = out.path().join("Luxforge");
        fs::create_dir(&dir).unwrap();
        desktop_integration(root.path(), &dir, "0.2.0-rc.1", "42").unwrap();
        assert_eq!(mode(&dir.join(SCRIPT)), 0o755);
        let template = dir.join(DESKTOP_DIR).join(TEMPLATE);
        assert_eq!(mode(&template), 0o644);
        let text = fs::read_to_string(&template).unwrap();
        for line in [
            "Exec=@EXEC@",
            "TryExec=@TRYEXEC@",
            "Icon=luxforge",
            "StartupWMClass=luxforge",
            "X-Luxforge-Installed-By=desktop-integration.sh",
            "X-Luxforge-Version=0.2.0-rc.1",
            "X-Luxforge-Build=42",
        ] {
            assert!(text.lines().any(|l| l == line), "{line}");
        }
        let icon = dir.join("desktop/icons/hicolor/256x256/apps/luxforge.png");
        assert_eq!(fs::read(&icon).unwrap(), committed_icon());
        assert_eq!(mode(&icon), 0o644);
        // Every size the helper knows, from the committed icons; the PNGs are the icon module's.
        let script = fs::read_to_string(dir.join(SCRIPT)).unwrap();
        for (file, hicolor) in ICONS {
            let installed = dir.join("desktop/icons/hicolor").join(hicolor);
            assert_eq!(
                fs::read(installed).unwrap(),
                fs::read(repository().join(icon::DIRECTORY).join(file)).unwrap()
            );
            assert!(script.contains(hicolor), "{hicolor}");
        }
        for (_, png) in icon::PNGS {
            assert!(ICONS.iter().any(|(file, _)| file == png), "{png}");
        }
        // Nothing in the payload names the checkout.
        let checkout = root.path().to_str().unwrap();
        for path in files(&dir).unwrap() {
            let bytes = fs::read(&path).unwrap();
            assert!(
                !String::from_utf8_lossy(&bytes).contains(checkout),
                "{path:?}"
            );
        }
        // The archive keeps the modes.
        let archive = out.path().join("payload.tar.gz");
        package::archive(&dir, &archive).unwrap();
        let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(
            fs::File::open(archive).unwrap(),
        ));
        let mut modes = std::collections::BTreeMap::new();
        for entry in archive.entries().unwrap() {
            let entry = entry.unwrap();
            let path = entry.path().unwrap().to_string_lossy().into_owned();
            modes.insert(path, entry.header().mode().unwrap() & 0o777);
        }
        assert_eq!(modes["Luxforge/desktop-integration.sh"], 0o755);
        assert_eq!(modes["Luxforge/desktop/luxforge.desktop.in"], 0o644);
    }

    #[test]
    fn missing_inputs_and_an_existing_integration_are_refused() {
        let out = tempfile::tempdir().unwrap();
        let dir = out.path().join("Luxforge");
        let without_icon = checkout(false);
        let refused = |root: &Path| {
            desktop_integration(root, &dir, "0.1.0", "1")
                .unwrap_err()
                .to_string()
        };
        assert!(refused(without_icon.path()).contains("Missing Linux payload: "));
        fs::create_dir(&dir).unwrap();
        let error = refused(without_icon.path());
        assert!(error.contains("packaging/icon/luxforge-256.png"), "{error}");
        let root = checkout(true);
        fs::remove_file(root.path().join(TEMPLATE_SOURCE)).unwrap();
        assert!(refused(root.path()).contains("luxforge.desktop.in"));
        let root = checkout(true);
        fs::write(root.path().join(TEMPLATE_SOURCE), "[Desktop Entry]\n").unwrap();
        assert!(refused(root.path()).contains("Exec=@EXEC@"));
        // Nothing was written by a refused attempt; a second attempt is refused.
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 0);
        let root = checkout(true);
        desktop_integration(root.path(), &dir, "0.1.0", "1").unwrap();
        assert!(refused(root.path()).contains("already has"));
        assert!(desktop_integration(root.path(), &dir, "0.1.0\nExec=x", "1").is_err());
    }

    #[test]
    fn the_help_names_requirements_integration_and_data() {
        let imports = json!({
            "glibc_baseline": "2.39",
            "max_glibc": "2.39",
            "executables": {
                "luxforge": {"needed": ["libgcc_s.so.1", "libm.so.6", "libc.so.6", "libstdc++.so.6"]},
                "luxforge-ctl": {"needed": ["libc.so.6"]},
            },
        });
        let help = help("0.1.0", &imports).unwrap();
        for text in [
            "luxforge-0.1.0-linux-x86_64.tar.gz",
            "./Luxforge/desktop-integration.sh install",
            "./Luxforge/desktop-integration.sh uninstall",
            "libvulkan.so.1",
            "xdg-desktop-portal",
            "libgcc_s.so.1, libm.so.6, libstdc++.so.6",
            "from libc6, libgcc-s1, libstdc++6",
            "glibc 2.39 at most (the baseline has 2.39)",
            "${XDG_CONFIG_HOME:-~/.config}/luxforge",
            "${XDG_STATE_HOME:-~/.local/state}/luxforge/logs",
            "never writes to your original photographs",
        ] {
            assert!(help.contains(text), "{text}");
        }
    }

    /// Install and uninstall in a payload and data home whose paths need quoting: spaces,
    /// non-ASCII, quotes, `$`, `` ` `` and parentheses, run from another directory.
    #[test]
    #[slow]
    fn install_writes_a_quoted_entry_and_uninstall_removes_only_its_own_files() {
        let root = checkout(true);
        let tmp = tempfile::tempdir().unwrap();
        let elsewhere = tmp.path().join("elsewhere");
        fs::create_dir(&elsewhere).unwrap();
        let dir = tmp.path().join("Photo apps ü \"q\" $HOME `x` (1)/Luxforge");
        payload(root.path(), &dir);
        let data = tmp.path().join("data ü home");
        let entry = data.join("applications/luxforge.desktop");
        let icon = data.join("icons/hicolor/256x256/apps/luxforge.png");
        // Someone else's files beside them, and Luxforge's data, which nothing may touch.
        let other = data.join("applications/other.desktop");
        let other_icon = data.join("icons/hicolor/256x256/apps/other.png");
        let catalog = data.join("luxforge/catalog.sqlite");
        for path in [&other, &other_icon, &catalog] {
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, b"kept").unwrap();
        }

        let output = run(&dir, "install", &data, &elsewhere);
        assert!(output.status.success(), "{output:?}");
        let text = fs::read_to_string(&entry).unwrap();
        let executable = dir.join("luxforge");
        let executable = executable.canonicalize().unwrap();
        assert_eq!(
            Path::new(&exec_program(key(&text, "Exec"))),
            executable,
            "{text}"
        );
        assert_eq!(Path::new(key(&text, "TryExec")), executable);
        assert_eq!(key(&text, "X-Luxforge-Build"), "7");
        assert!(!text.contains('@'), "{text}");
        assert_eq!(mode(&entry), 0o644);
        assert_eq!(fs::read(&icon).unwrap(), committed_icon());
        // Luxforge's updater recognises the entry as this payload's and keeps its encoding.
        let build = luxforge_update::payload::BuildInfo::parse(
            r#"{"version":"0.2.0","build_number":"8","target":"x86_64-unknown-linux-gnu"}"#,
        )
        .unwrap();
        assert!(
            luxforge_update::payload::refresh_desktop_entry(
                &entry,
                executable.parent().unwrap(),
                &executable,
                &build
            )
            .unwrap()
        );
        let refreshed = fs::read_to_string(&entry).unwrap();
        assert_eq!(key(&refreshed, "Exec"), key(&text, "Exec"));
        assert_eq!(key(&refreshed, "X-Luxforge-Build"), "8");
        // Installing again replaces its own entry.
        assert!(run(&dir, "install", &data, &elsewhere).status.success());

        let output = run(&dir, "uninstall", &data, &elsewhere);
        assert!(output.status.success(), "{output:?}");
        assert!(!entry.exists() && !icon.exists());
        for path in [&other, &other_icon, &catalog] {
            assert_eq!(fs::read(path).unwrap(), b"kept");
        }
        assert!(dir.join("luxforge").is_file() && dir.join("build.json").is_file());
        // Uninstalling again finds nothing to remove.
        assert!(run(&dir, "uninstall", &data, &elsewhere).status.success());

        // An entry install did not write is never replaced or removed.
        fs::write(&entry, "[Desktop Entry]\nExec=/opt/luxforge/luxforge\n").unwrap();
        for command in ["install", "uninstall"] {
            let output = run(&dir, command, &data, &elsewhere);
            assert!(!output.status.success(), "{command}");
            assert_eq!(
                fs::read_to_string(&entry).unwrap(),
                "[Desktop Entry]\nExec=/opt/luxforge/luxforge\n"
            );
        }
        assert!(!icon.exists());
    }

    #[test]
    #[slow]
    fn the_helper_refuses_a_folder_that_is_not_a_payload_or_cannot_be_written_safely() {
        let root = checkout(true);
        let tmp = tempfile::tempdir().unwrap();
        let data = tmp.path().join("data");
        // The script alone, outside an extracted payload.
        let lone = tmp.path().join("lone");
        fs::create_dir(&lone).unwrap();
        fs::copy(root.path().join(SCRIPT_SOURCE), lone.join(SCRIPT)).unwrap();
        let output = run(&lone, "install", &data, tmp.path());
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("not an extracted Luxforge"));
        // A backslash or line break cannot be written back by the updater, and GLib cannot launch
        // a program whose path holds a %.
        for name in ["back\\slash", "line\nbreak", "100%"] {
            let dir = tmp.path().join(name).join("Luxforge");
            payload(root.path(), &dir);
            let output = run(&dir, "install", &data, tmp.path());
            assert!(!output.status.success(), "{name}");
        }
        assert!(!data.exists(), "a refused install writes nothing");
        // An unknown command prints the usage and fails; help succeeds and names the data.
        let dir = tmp.path().join("plain/Luxforge");
        payload(root.path(), &dir);
        assert_eq!(
            run(&dir, "remove", &data, tmp.path()).status.code(),
            Some(2)
        );
        let output = run(&dir, "help", &data, tmp.path());
        assert!(output.status.success());
        let usage = String::from_utf8_lossy(&output.stdout);
        assert!(
            usage.contains("${XDG_CONFIG_HOME:-~/.config}/luxforge"),
            "{usage}"
        );
        assert!(!data.exists());
    }
}
