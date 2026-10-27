//! installer/light-view.iss is compiled only by the release workflow (Inno Setup is not
//! installed locally, ruling R4), so this checks the directives it must keep.

use std::path::PathBuf;

const EXTENSIONS: [&str; 8] = ["jpg", "jpeg", "png", "gif", "webp", "bmp", "heic", "heif"];

fn script() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../installer/light-view.iss");
    let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    // Inno Setup reads a script as UTF-8 only with a BOM; the Korean messages need it.
    let text = bytes
        .strip_prefix(b"\xEF\xBB\xBF")
        .expect("light-view.iss starts with a UTF-8 BOM");
    String::from_utf8(text.to_vec()).expect("UTF-8")
}

/// The script's lines without comments, in the section `[name]`.
fn section(script: &str, name: &str) -> Vec<String> {
    let header = format!("[{name}]");
    script
        .lines()
        .map(str::trim)
        .skip_while(|l| !l.eq_ignore_ascii_case(&header))
        .skip(1)
        .take_while(|l| !l.starts_with('['))
        .filter(|l| !l.is_empty() && !l.starts_with(';'))
        .map(str::to_owned)
        .collect()
}

fn has_line(lines: &[String], parts: &[&str]) -> bool {
    lines.iter().any(|l| parts.iter().all(|p| l.contains(p)))
}

#[test]
fn installer_is_per_user_in_the_spec_folder() {
    let s = script();
    let setup = section(&s, "Setup");
    assert!(setup.contains(&"PrivilegesRequired=lowest".to_owned()));
    assert!(setup.contains(&r"DefaultDirName={localappdata}\Programs\light-view".to_owned()));
    assert!(setup.contains(&"ChangesAssociations=yes".to_owned()));
    assert!(
        !setup
            .iter()
            .any(|l| l.starts_with("PrivilegesRequiredOverridesAllowed")),
        "no all-users choice"
    );
    // Every registry entry goes to the current user.
    let registry = section(&s, "Registry");
    assert!(!registry.is_empty());
    assert!(registry.iter().all(|l| l.starts_with("Root: HKCU;")));
}

#[test]
fn installer_registers_every_extension() {
    let s = script();
    let registry = section(&s, "Registry");
    for ext in EXTENSIONS {
        let prog_id = format!("goodflow.lightview.{ext}");
        let classes = format!(r#"Subkey: "Software\Classes\{prog_id}""#);
        let command = format!(r#"Subkey: "Software\Classes\{prog_id}\shell\open\command""#);
        let open_with = format!(r#"Subkey: "Software\Classes\.{ext}\OpenWithProgids""#);
        let supported = r#"Subkey: "Software\Classes\Applications\{#AppExe}\SupportedTypes""#;
        assert!(
            has_line(&registry, &[&classes, "uninsdeletekey"]),
            "{prog_id}"
        );
        assert!(has_line(&registry, &[&command, "%1"]), "{prog_id} command");
        assert!(
            has_line(
                &registry,
                &[
                    &open_with,
                    &format!(r#"ValueName: "{prog_id}""#),
                    "uninsdeletevalue"
                ]
            ),
            ".{ext} OpenWithProgids"
        );
        assert!(
            has_line(&registry, &[supported, &format!(r#"ValueName: ".{ext}""#)]),
            ".{ext} SupportedTypes"
        );
        assert!(
            has_line(
                &registry,
                &[
                    r#"Subkey: "{#Capabilities}\FileAssociations""#,
                    &format!(r#"ValueName: ".{ext}"; ValueData: "{prog_id}""#)
                ]
            ),
            ".{ext} default apps capability"
        );
    }
    // The default apps page finds light-view under this name (registeredAppUser=light-view).
    assert!(has_line(
        &registry,
        &[
            r#"Subkey: "Software\RegisteredApplications""#,
            r#"ValueName: "light-view""#
        ]
    ));
    assert!(s.contains(r#"#define AppExe "light-view.exe""#));
}

#[test]
fn installer_shortcut_has_app_user_model_id() {
    let s = script();
    assert!(s.contains(r#"#define AppUserModelID "goodflow.LightView""#));
    let icons = section(&s, "Icons");
    assert!(has_line(
        &icons,
        &[
            r#"Name: "{autoprograms}\light-view""#,
            r#"AppUserModelID: "{#AppUserModelID}""#
        ]
    ));
}

#[test]
fn installer_grants_appcontainer_read_and_cleans_up() {
    let s = script();
    let run = section(&s, "Run");
    // Ruling R2: ALL APPLICATION PACKAGES may read and run the install folder.
    assert!(has_line(
        &run,
        &[
            r#"Filename: "{sys}\icacls.exe""#,
            r#"""{app}"" /grant *S-1-15-2-1:(OI)(CI)(RX)"#,
            "waituntilterminated"
        ]
    ));
    let icacls = run.iter().position(|l| l.contains("icacls")).unwrap();
    assert!(
        !run[icacls].contains("postinstall"),
        "runs with the install, not as a choice"
    );
    // Last page: the default apps settings.
    assert!(has_line(
        &run,
        &[
            r#"Filename: "ms-settings:defaultapps?registeredAppUser=light-view""#,
            "postinstall",
            "shellexec"
        ]
    ));
    let uninstall = section(&s, "UninstallRun");
    assert!(has_line(
        &uninstall,
        &[
            r#"Filename: "{app}\{#AppExe}""#,
            r#"Parameters: "--uninstall-cleanup""#,
            "runhidden",
            "waituntilterminated",
            "RunOnceId:"
        ]
    ));
}

#[test]
fn installer_never_ships_the_portable_marker() {
    let s = script();
    let files = section(&s, "Files");
    assert!(has_line(
        &files,
        &[r#"Source: "{#StageDir}\*""#, r#"Excludes: "portable.txt""#]
    ));
    let delete = section(&s, "InstallDelete");
    assert!(has_line(&delete, &[r#"Name: "{app}\portable.txt""#]));
}
