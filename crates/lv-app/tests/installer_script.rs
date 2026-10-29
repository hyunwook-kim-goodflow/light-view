//! installer/light-view.iss is compiled only by the release workflow (Inno Setup is not
//! installed locally, ruling R4), so this checks the directives it must keep. The same goes
//! for scripts/package.ps1 and .github/workflows/release.yml, which run only in CI.

use std::path::PathBuf;

const EXTENSIONS: [&str; 8] = ["jpg", "jpeg", "png", "gif", "webp", "bmp", "heic", "heif"];
/// The types the Explorer extension (crates/lv-shell) handles (M7 Global Constraints).
const SHELL_EXTENSIONS: [&str; 2] = ["heic", "heif"];
/// The versioned shell DLL (spec M7 §6): every update installs a new file name.
const SHELL_DLL: &str = r"{app}\lv-shell-{#AppVersion}.dll";
/// Entries only an all-users (administrator) install makes.
const ADMIN_ONLY: &str = "; Check: IsAdminInstallMode";
/// Entries only a per-user install makes.
const PER_USER_ONLY: &str = "; Check: not IsAdminInstallMode";

fn repo_file(relative: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(relative)
}

fn read_text(relative: &str) -> String {
    let path = repo_file(relative);
    let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let text = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&bytes);
    String::from_utf8(text.to_vec()).expect("UTF-8")
}

fn script() -> String {
    let path = repo_file("installer/light-view.iss");
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

/// The [Code] section (Pascal), without `//` comment lines.
fn code(script: &str) -> String {
    let code = script
        .split_once("\n[Code]")
        .or_else(|| script.split_once("\r\n[Code]"))
        .expect("[Code] section")
        .1;
    code.lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The body of the Pascal routine `name` (from its header to the next routine).
fn routine(code: &str, name: &str) -> String {
    let start = code
        .find(&format!("procedure {name}("))
        .or_else(|| code.find(&format!("function {name}(")))
        .unwrap_or_else(|| panic!("routine {name} in [Code]"));
    let rest = &code[start..];
    let end = rest[1..]
        .find("\nprocedure ")
        .into_iter()
        .chain(rest[1..].find("\nfunction "))
        .min()
        .map_or(rest.len(), |i| i + 1);
    rest[..end].to_owned()
}

fn has_line(lines: &[String], parts: &[&str]) -> bool {
    lines.iter().any(|l| parts.iter().all(|p| l.contains(p)))
}

/// The one [Registry] line that writes `value` (`""` = the default value) of `subkey`.
fn value_line<'a>(registry: &'a [String], subkey: &str, value: &str) -> &'a str {
    let key = format!(r#"Subkey: "{subkey}";"#);
    let name = format!(r#"ValueName: "{value}";"#);
    let found: Vec<&String> = registry
        .iter()
        .filter(|l| l.contains(&key) && l.contains(&name))
        .collect();
    assert_eq!(found.len(), 1, "{subkey} / {value:?}: {found:?}");
    found[0]
}

/// `{XXXXXXXX-XXXX-XXXX-XXXX-XXXXXXXXXXXX}` of the `GUID::from_u128` constant `name` in
/// crates/lv-shell/src/lib.rs, so the installer registers the classes the DLL serves.
fn shell_clsid(name: &str) -> String {
    let lib = read_text("crates/lv-shell/src/lib.rs");
    let decl = format!("pub const {name}: GUID = GUID::from_u128(0x");
    let start = lib
        .find(&decl)
        .unwrap_or_else(|| panic!("{name} in lv-shell"))
        + decl.len();
    let hex: Vec<&str> = lib[start..].split(')').next().unwrap().split('_').collect();
    assert_eq!(hex.len(), 5, "{name}");
    format!("{{{}}}", hex.join("-"))
}

#[test]
fn installer_offers_per_user_and_all_users() {
    let s = script();
    let setup = section(&s, "Setup");
    // Spec M7 §2: per-user by default, all users (administrator) on the first page.
    assert!(setup.contains(&"PrivilegesRequired=lowest".to_owned()));
    assert!(setup.contains(&"PrivilegesRequiredOverridesAllowed=dialog".to_owned()));
    // {autopf}: %LOCALAPPDATA%\Programs per user, C:\Program Files for all users.
    assert!(setup.contains(&r"DefaultDirName={autopf}\light-view".to_owned()));
    assert!(setup.contains(&"ChangesAssociations=yes".to_owned()));
    // The folder is fixed: the icacls grant must not reach a folder the user picked.
    assert!(setup.contains(&"DisableDirPage=yes".to_owned()));
    // Every registry entry follows the chosen mode: HKA is HKCU per user, HKLM for all.
    let registry = section(&s, "Registry");
    assert!(!registry.is_empty());
    for line in &registry {
        assert!(line.starts_with("Root: HKA;"), "{line}");
    }
    // The spec folder of each mode (IsDefaultAppDir guards the icacls grant).
    let code = code(&s);
    let default_dir = routine(&code, "IsDefaultAppDir");
    assert!(default_dir.contains("function IsDefaultAppDir(): Boolean;"));
    assert!(default_dir.contains("if IsAdminInstallMode then"));
    assert!(default_dir.contains(r"Expected := ExpandConstant('{autopf}\light-view')"));
    assert!(
        default_dir.contains(r"Expected := ExpandConstant('{localappdata}\Programs\light-view')")
    );
    assert!(
        default_dir.contains("Installed := RemoveBackslashUnlessRoot(ExpandConstant('{app}'));")
    );
    // CompareText ignores case.
    assert!(default_dir
        .contains("Result := CompareText(Installed, RemoveBackslashUnlessRoot(Expected)) = 0;"));
    // One line when the other mode is installed too (spec §6): no forced removal.
    let app_id = setup
        .iter()
        .find_map(|l| l.strip_prefix("AppId={"))
        .expect("AppId");
    assert!(code.contains(&format!(
        r"UninstallKey = 'Software\Microsoft\Windows\CurrentVersion\Uninstall\{app_id}_is1';"
    )));
    let setup_init = routine(&code, "InitializeSetup");
    assert!(setup_init.contains("RegKeyExists(HKCU, UninstallKey)"));
    assert!(setup_init.contains("RegKeyExists(HKLM, UninstallKey)"));
    assert!(setup_init.contains("SuppressibleMsgBox(CustomMessage('PerUserAlsoInstalled')"));
    assert!(setup_init.contains("SuppressibleMsgBox(CustomMessage('AllUsersAlsoInstalled')"));
    assert!(setup_init.contains("Result := True;"));
    let messages = section(&s, "CustomMessages");
    for name in ["PerUserAlsoInstalled", "AllUsersAlsoInstalled"] {
        assert!(has_line(&messages, &[&format!("korean.{name}=")]), "{name}");
        assert!(
            has_line(&messages, &[&format!("english.{name}=")]),
            "{name}"
        );
    }
    // Last page: the default apps page for the hive the app is registered in.
    let run = section(&s, "Run");
    assert!(has_line(
        &run,
        &[
            r#"Filename: "ms-settings:defaultapps?registeredAppUser=light-view""#,
            "postinstall",
            "shellexec",
            PER_USER_ONLY
        ]
    ));
    assert!(has_line(
        &run,
        &[
            r#"Filename: "ms-settings:defaultapps?registeredAppMachine=light-view""#,
            "postinstall",
            "shellexec",
            ADMIN_ONLY
        ]
    ));
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
    // Uninstalling removes only our value under a shared `.<ext>` key (or its OpenWithProgids
    // or ShellEx key), never the key: it holds other programs' registrations. Our own
    // handler subkeys under ShellEx are checked in installer_registers_heic_shell_handlers.
    for line in &registry {
        let shared = EXTENSIONS.iter().any(|ext| {
            [
                format!(r#"Subkey: "Software\Classes\.{ext}";"#),
                format!(r#"Subkey: "Software\Classes\.{ext}\OpenWithProgids";"#),
                format!(r#"Subkey: "Software\Classes\.{ext}\ShellEx";"#),
            ]
            .iter()
            .any(|key| line.contains(key))
        });
        if shared {
            assert!(!line.contains("uninsdeletekey"), "{line}");
            assert!(line.contains("uninsdeletevalue"), "{line}");
        }
    }
    // The default apps page finds light-view under this name (registeredAppUser/Machine).
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
    // Review I1: /DIR= or a previous install's folder bypasses DisableDirPage, so the
    // grant runs only when {app} is the spec folder. M7: and only per user (Program Files
    // can already be read by AppContainers).
    assert!(
        run[icacls].ends_with("; Check: IsDefaultAppDir and not IsAdminInstallMode"),
        "{}",
        run[icacls]
    );
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
    // The uninstaller takes the grant back, under the same check.
    assert!(has_line(
        &uninstall,
        &[
            r#"Filename: "{sys}\icacls.exe""#,
            r#"""{app}"" /remove:g *S-1-15-2-1"#,
            "waituntilterminated",
            "RunOnceId:",
            "; Check: IsDefaultAppDir and not IsAdminInstallMode"
        ]
    ));
}

#[test]
fn installer_registers_heic_shell_handlers() {
    let s = script();
    // The class IDs are the ones the DLL serves.
    let classes = [
        ("ThumbnailClsid", "CLSID_THUMBNAIL"),
        ("PreviewClsid", "CLSID_PREVIEW"),
        ("DetailsClsid", "CLSID_DETAILS"),
    ];
    for (define, constant) in classes {
        let clsid = shell_clsid(constant);
        // `{{` is Inno Setup's escaped `{`.
        assert!(
            s.contains(&format!(r#"#define {define} "{{{clsid}""#)),
            "{define} = {clsid}"
        );
    }
    assert_eq!(
        shell_clsid("CLSID_THUMBNAIL"),
        "{6A77F04F-33D3-40F0-885E-AA8CC5CC403C}"
    );
    assert!(s.contains(r#"#define ThumbnailShellEx "{{e357fccd-a995-4576-b01f-234630154e96}""#));
    assert!(s.contains(r#"#define PreviewShellEx "{{8895b1c6-b41f-4c1c-a562-0d564250836f}""#));
    // The 64-bit preview host (prevhost.exe).
    assert!(s.contains(r#"#define PrevhostAppId "{{6d2b5079-2f0b-48dd-ab7f-97cec514d30b}""#));

    let registry = section(&s, "Registry");
    let clsid_key = |define: &str| format!(r"Software\Classes\CLSID\{{#{define}}}");
    // Each class: its key goes on uninstall, the in-process server is this version's DLL.
    for (define, _) in classes {
        let key = clsid_key(define);
        let line = value_line(&registry, &key, "");
        assert!(line.contains("uninsdeletekey"), "{line}");
        let server = format!(r"{key}\InprocServer32");
        let line = value_line(&registry, &server, "");
        assert!(
            line.contains(&format!(r#"ValueData: "{SHELL_DLL}""#)),
            "{line}"
        );
        let line = value_line(&registry, &server, "ThreadingModel");
        assert!(line.contains(r#"ValueData: "Apartment""#), "{line}");
    }
    // Thumbnails in both modes; per user they run inside Explorer (Task 1 spike).
    for line in registry.iter().filter(|l| l.contains("{#ThumbnailClsid}")) {
        if line.contains("DisableProcessIsolation") {
            assert!(line.ends_with(PER_USER_ONLY), "{line}");
        } else {
            assert!(!line.contains("Check:"), "{line}");
        }
    }
    let isolation = value_line(
        &registry,
        &clsid_key("ThumbnailClsid"),
        "DisableProcessIsolation",
    );
    assert!(isolation.contains(r#"ValueType: dword"#), "{isolation}");
    assert!(isolation.contains(r#"ValueData: "1""#), "{isolation}");
    assert_eq!(
        registry
            .iter()
            .filter(|l| l.contains("DisableProcessIsolation"))
            .count(),
        1
    );
    // Preview (prevhost reads HKLM only, Task 1b) and details (HKLM PropertySystem) are
    // registered by all-users installs only.
    for line in &registry {
        let admin = ["{#PreviewClsid}", "{#DetailsClsid}", "{#PreviewShellEx}"]
            .iter()
            .any(|d| line.contains(d))
            || line.contains("PreviewHandlers")
            || line.contains("PropertySystem");
        if admin {
            assert!(line.ends_with(ADMIN_ONLY), "{line}");
        }
    }
    let app_id = value_line(&registry, &clsid_key("PreviewClsid"), "AppID");
    assert!(
        app_id.contains(r#"ValueData: "{#PrevhostAppId}""#),
        "{app_id}"
    );
    let handlers = value_line(
        &registry,
        r"Software\Microsoft\Windows\CurrentVersion\PreviewHandlers",
        "{#PreviewClsid}",
    );
    assert!(
        handlers.contains(r#"ValueData: "light-view HEIC preview""#),
        "{handlers}"
    );
    assert!(handlers.contains("uninsdeletevalue"), "{handlers}");
    assert!(!handlers.contains("uninsdeletekey"), "{handlers}");
    for ext in SHELL_EXTENSIONS {
        // Only our handler's subkey is deleted, never `.heic` or its ShellEx key.
        let thumb = value_line(
            &registry,
            &format!(r"Software\Classes\.{ext}\ShellEx\{{#ThumbnailShellEx}}"),
            "",
        );
        assert!(
            thumb.contains(r#"ValueData: "{#ThumbnailClsid}""#),
            "{thumb}"
        );
        assert!(thumb.contains("uninsdeletekey"), "{thumb}");
        let preview = value_line(
            &registry,
            &format!(r"Software\Classes\.{ext}\ShellEx\{{#PreviewShellEx}}"),
            "",
        );
        assert!(
            preview.contains(r#"ValueData: "{#PreviewClsid}""#),
            "{preview}"
        );
        assert!(preview.contains("uninsdeletekey"), "{preview}");
        // The details handler replaces Windows' own; [Code] saves the previous value and
        // puts it back on uninstall, so Inno Setup must not delete it.
        let details = value_line(
            &registry,
            &format!(
                r"Software\Microsoft\Windows\CurrentVersion\PropertySystem\PropertyHandlers\.{ext}"
            ),
            "",
        );
        assert!(
            details.contains(r#"ValueData: "{#DetailsClsid}""#),
            "{details}"
        );
        assert!(!details.contains("uninsdelete"), "{details}");
    }

    let code = code(&s);
    assert!(code.contains(r"'Software\goodflow\light-view\PreviousPropertyHandler'"));
    assert!(code
        .contains(r"'Software\Microsoft\Windows\CurrentVersion\PropertySystem\PropertyHandlers'"));
    // Saved before [Registry] writes ours (ssInstall), never overwritten with our own class
    // on a reinstall; put back before the uninstaller deletes our keys (usUninstall), only
    // while the value is still ours; deleted when there was none.
    let save = routine(&code, "SavePropertyHandler");
    assert!(save.contains("RegQueryStringValue(HKA, PropertyHandlers + '\\' + Ext, '', Current)"));
    assert!(save.contains("CompareText(Current, DetailsClass()) = 0"));
    assert!(save.contains("RegWriteStringValue(HKA, SavedHandlers + '\\' + Ext, '', Current)"));
    let restore = routine(&code, "RestorePropertyHandler");
    assert!(restore.contains("CompareText(Current, DetailsClass()) <> 0"));
    assert!(restore.contains("RegQueryStringValue(HKA, SavedHandlers + '\\' + Ext, '', Previous)"));
    assert!(restore.contains("RegWriteStringValue(HKA, Key, '', Previous)"));
    assert!(restore.contains("RegDeleteValue(HKA, Key, '')"));
    let step = routine(&code, "CurStepChanged");
    assert!(step.contains("if (CurStep = ssInstall) and IsAdminInstallMode then"));
    for ext in SHELL_EXTENSIONS {
        assert!(
            step.contains(&format!("SavePropertyHandler('.{ext}');")),
            "{ext}"
        );
    }
    let uninstall = routine(&code, "CurUninstallStepChanged");
    assert!(uninstall.contains("if (CurUninstallStep = usUninstall) and IsAdminInstallMode then"));
    for ext in SHELL_EXTENSIONS {
        assert!(
            uninstall.contains(&format!("RestorePropertyHandler('.{ext}');")),
            "{ext}"
        );
    }
    let class = routine(&code, "DetailsClass");
    assert!(class.contains("Result := ExpandConstant('{#DetailsClsid}');"));
}

#[test]
fn installer_uses_versioned_shell_dll_and_runonce_cleanup() {
    let s = script();
    let files = section(&s, "Files");
    // The DLL has its own entry: the wildcard leaves it out, and a file Explorer may hold
    // (same version reinstalled) is never overwritten.
    assert!(has_line(
        &files,
        &[r#"Source: "{#StageDir}\*""#, "lv-shell-*.dll"]
    ));
    let dll = files
        .iter()
        .find(|l| l.contains(r#"Source: "{#StageDir}\lv-shell-{#AppVersion}.dll""#))
        .expect("shell DLL entry");
    assert!(dll.contains(r#"DestDir: "{app}""#), "{dll}");
    assert!(dll.contains("onlyifdoesntexist"), "{dll}");
    let registry = section(&s, "Registry");
    assert_eq!(
        registry
            .iter()
            .filter(|l| l.contains(&format!(r#"ValueData: "{SHELL_DLL}""#)))
            .count(),
        3,
        "thumbnail, preview and details servers"
    );
    assert!(!s.contains("lv_shell.dll"), "the staged name is versioned");

    // Other versions' DLLs are deleted after the new one is registered (ssPostInstall) and
    // on uninstall; a DLL Explorer still holds goes at the next logon (RunOnce in HKA:
    // HKCU per user, HKLM for all users).
    let code = code(&s);
    assert!(code.contains("'lv-shell-{#AppVersion}.dll'"));
    let delete = routine(&code, "DeleteShellDlls");
    assert!(delete.contains(r"FindFirst(Dir + '\lv-shell-*.dll', Found)"));
    assert!(delete.contains("if not DeleteFile(Path) then"));
    assert!(delete.contains("DeleteAtNextLogon(Path"));
    let later = routine(&code, "DeleteAtNextLogon");
    assert!(later.contains("RegWriteStringValue(HKA, RunOnceKey,"));
    assert!(later.contains("/c del /f /q \"' + Path + '\"'"));
    assert!(later.contains("ExpandConstant('{cmd}')"));
    assert!(code.contains(r"RunOnceKey = 'Software\Microsoft\Windows\CurrentVersion\RunOnce';"));
    let step = routine(&code, "CurStepChanged");
    assert!(step.contains("if CurStep = ssPostInstall then"));
    assert!(step.contains("DeleteShellDlls(False);"));
    let uninstall = routine(&code, "CurUninstallStepChanged");
    assert!(uninstall.contains("if CurUninstallStep = usPostUninstall then"));
    assert!(uninstall.contains("DeleteShellDlls(True);"));
    // A reinstall of the same version cancels a pending delete of its own DLL.
    assert!(delete.contains("RegDeleteValue(HKA, RunOnceKey, RunOnceName(CurrentShellDll()))"));

    // Spec §6: never close or restart Explorer. Inno Setup's CloseApplications defaults
    // to yes (Restart Manager would offer to close Explorer for a DLL it holds).
    let setup = section(&s, "Setup");
    assert!(setup.contains(&"CloseApplications=no".to_owned()));
    // (Comments may explain why; entries and code must not do it.)
    let lower = s
        .lines()
        .map(str::trim_start)
        .filter(|l| !l.starts_with(';') && !l.starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n")
        .to_ascii_lowercase();
    for banned in [
        "closeapplications=yes",
        "closeapplications=force",
        "restartapplications=yes",
        "taskkill",
        "explorer.exe",
        "alwaysrestart",
        "restartreplace",
    ] {
        assert!(!lower.contains(banned), "{banned}");
    }
}

#[test]
fn installer_never_ships_the_portable_marker() {
    let s = script();
    let files = section(&s, "Files");
    let all = files
        .iter()
        .find(|l| l.contains(r#"Source: "{#StageDir}\*""#))
        .expect("stage wildcard");
    let excludes = all
        .split("Excludes: \"")
        .nth(1)
        .and_then(|r| r.split('"').next())
        .expect("Excludes");
    assert!(excludes.split(',').any(|e| e == "portable.txt"), "{all}");
    let delete = section(&s, "InstallDelete");
    assert!(has_line(&delete, &[r#"Name: "{app}\portable.txt""#]));
}

#[test]
fn installer_registers_resize_verb_for_all_extensions() {
    let s = script();
    let registry = section(&s, "Registry");
    for ext in EXTENSIONS {
        let verb = format!(
            r"Software\Classes\SystemFileAssociations\.{ext}\shell\goodflow.lightview.resize"
        );
        let key = format!(r#"Subkey: "{verb}""#);
        let command = format!(r#"Subkey: "{verb}\command""#);
        // Default value is the menu text ({cm:}, Korean or English); the key goes on uninstall.
        assert!(
            has_line(
                &registry,
                &[
                    &key,
                    r#"ValueName: """#,
                    "{cm:ResizeVerb}",
                    "uninsdeletekey"
                ]
            ),
            ".{ext} verb text"
        );
        assert!(
            has_line(
                &registry,
                &[
                    &key,
                    r#"ValueName: "MultiSelectModel""#,
                    r#"ValueData: "Player""#
                ]
            ),
            ".{ext} MultiSelectModel"
        );
        assert!(
            has_line(
                &registry,
                &[
                    &key,
                    r#"ValueName: "Icon""#,
                    r#"ValueData: "{app}\{#AppExe}""#
                ]
            ),
            ".{ext} icon"
        );
        // Run once per file: `"<exe>" --resize "%1"`.
        assert!(
            has_line(
                &registry,
                &[
                    &command,
                    r#"ValueData: """{app}\{#AppExe}"" --resize ""%1""""#
                ]
            ),
            ".{ext} command"
        );
    }
    // Only our own verb keys are deleted, never a shared SystemFileAssociations key.
    for line in &registry {
        if line.contains("SystemFileAssociations") {
            assert!(line.contains("goodflow.lightview.resize"), "{line}");
        }
    }
    let messages = section(&s, "CustomMessages");
    assert!(has_line(
        &messages,
        &["korean.ResizeVerb=", "light-view로 크기 조정"]
    ));
    assert!(has_line(
        &messages,
        &["english.ResizeVerb=Resize with light-view"]
    ));
}

/// Photos show light-view's file icon (the exe's second icon group, `build.rs` id 2)
/// when Explorer has no thumbnail, and the installer carries the app icon.
#[test]
fn installer_uses_light_view_icons() {
    let s = script();
    let setup = section(&s, "Setup");
    assert!(setup.contains(&r"SetupIconFile=..\crates\lv-app\res\light-view.ico".to_owned()));
    let icon = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("res/light-view.ico");
    assert!(icon.is_file(), "{} exists", icon.display());
    let registry = section(&s, "Registry");
    for ext in EXTENSIONS {
        let key = format!(r"goodflow.lightview.{ext}\DefaultIcon");
        assert!(
            has_line(&registry, &[&key, r#"ValueData: "{app}\{#AppExe},1""#]),
            "{ext} uses the file icon"
        );
    }
}

/// scripts/package.ps1 puts the shell DLL, renamed per version, into the installer's
/// input only (the portable copy registers nothing), and checks that it imports Windows
/// DLLs only (the shipped DLL is the gate for Task 4's import test).
#[test]
fn package_ships_versioned_shell_dll_in_installer_only() {
    let ps = read_text("scripts/package.ps1");
    assert!(ps.contains("'-p', 'lv-shell'"), "built with the exes");
    assert!(ps.contains("Join-Path $release 'lv_shell.dll'"));
    assert!(ps.contains("Assert-NoVcRuntimeImport $shellDll"));
    assert!(ps.contains("Assert-ShellDllImportsWindowsOnly $shellDll"));
    assert!(ps.contains(r#"(Join-Path $StageDir "lv-shell-$Version.dll")"#));
    assert!(
        ps.contains("-Exclude 'lv-shell-*.dll'"),
        "not in the portable zip"
    );
    assert!(ps.contains("is in the portable zip"));
    // The C libraries and the dynamic C runtime are refused by name; anything else must be
    // a Windows DLL (System32 or an API set).
    let check = ps
        .split("function Assert-ShellDllImportsWindowsOnly")
        .nth(1)
        .expect("Assert-ShellDllImportsWindowsOnly");
    let check = check.split("\nfunction ").next().unwrap();
    assert!(check.contains("$NativeDlls"));
    assert!(check.contains("$vcpkgDlls"));
    assert!(check.contains("api-ms-win-crt-"));
    assert!(check.contains("ucrtbase"));
    assert!(check.contains("System32"));
}

/// When signing is set up, the release workflow signs the shell DLL with the exes, before
/// package.ps1 stages it.
#[test]
fn release_builds_and_signs_shell_dll() {
    let yml = read_text(".github/workflows/release.yml");
    assert!(yml.contains("cargo build --release --locked -p lv-app -p lv-worker -p lv-shell"));
    assert!(yml.contains("target/release/lv_shell.dll"));
    assert!(yml.contains("signed-exes/lv_shell.dll"));
}
