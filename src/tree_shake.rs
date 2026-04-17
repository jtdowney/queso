use std::collections::{HashMap, HashSet, VecDeque};

use eyre::{Result, bail};

const IMPLICIT_APPS: &[&str] = &["kernel", "stdlib", "compiler"];

pub fn resolve(
    shipment_apps: &HashMap<String, Vec<String>>,
    erts_apps: &HashMap<String, Vec<String>>,
) -> HashSet<String> {
    let mut required = HashSet::new();
    let mut queue: VecDeque<String> = IMPLICIT_APPS
        .iter()
        .copied()
        .map(String::from)
        .chain(shipment_apps.keys().cloned())
        .chain(shipment_apps.values().flatten().cloned())
        .collect();

    while let Some(app) = queue.pop_front() {
        if !required.insert(app.clone()) {
            continue;
        }
        if let Some(deps) = erts_apps.get(&app) {
            for dep in deps {
                if !required.contains(dep) {
                    queue.push_back(dep.clone());
                }
            }
        }
    }

    required
}

pub fn validate(
    required: &HashSet<String>,
    shipment_apps: &HashMap<String, Vec<String>>,
    erts_apps: &HashMap<String, Vec<String>>,
) -> Result<()> {
    let mut missing: Vec<&str> = required
        .iter()
        .filter(|app| {
            !shipment_apps.contains_key(app.as_str()) && !erts_apps.contains_key(app.as_str())
        })
        .map(String::as_str)
        .collect();

    if missing.is_empty() {
        return Ok(());
    }

    missing.sort_unstable();
    bail!(
        "required OTP app(s) not found in ERTS: {}. Try a different target or use --full-erts",
        missing.join(", ")
    );
}

#[cfg(test)]
mod test {
    use quickcheck_macros::quickcheck;

    use super::*;

    fn apps(entries: &[(&str, &[&str])]) -> HashMap<String, Vec<String>> {
        entries
            .iter()
            .map(|(name, deps)| {
                (
                    (*name).to_string(),
                    deps.iter().map(|d| (*d).to_string()).collect(),
                )
            })
            .collect()
    }

    #[test]
    fn test_resolve_minimal_gleam_app() {
        let shipment_apps = apps(&[("my_app", &["gleam_stdlib"]), ("gleam_stdlib", &[])]);
        let erts_apps = apps(&[
            ("kernel", &[]),
            ("stdlib", &["kernel"]),
            ("compiler", &["kernel", "stdlib"]),
            ("crypto", &["kernel", "stdlib"]),
            ("snmp", &["kernel", "stdlib"]),
        ]);

        let required = resolve(&shipment_apps, &erts_apps);
        assert!(required.contains("kernel"));
        assert!(required.contains("stdlib"));
        assert!(required.contains("compiler"));
        assert!(required.contains("my_app"));
        assert!(required.contains("gleam_stdlib"));
        assert!(!required.contains("crypto"));
        assert!(!required.contains("snmp"));
    }

    #[test]
    fn test_resolve_with_transitive_otp_deps() {
        let shipment_apps = apps(&[("my_app", &["my_lib"]), ("my_lib", &["crypto"])]);
        let erts_apps = apps(&[
            ("kernel", &[]),
            ("stdlib", &["kernel"]),
            ("compiler", &["kernel", "stdlib"]),
            ("crypto", &["kernel", "stdlib"]),
            ("snmp", &["kernel", "stdlib"]),
        ]);

        let required = resolve(&shipment_apps, &erts_apps);
        assert!(required.contains("my_app"));
        assert!(required.contains("my_lib"));
        assert!(required.contains("crypto"));
        assert!(!required.contains("snmp"));
    }

    #[test]
    fn test_validate_passes_when_all_apps_available() {
        let shipment_apps = apps(&[("my_app", &["gleam_stdlib"]), ("gleam_stdlib", &[])]);
        let erts_apps = apps(&[
            ("kernel", &[]),
            ("stdlib", &["kernel"]),
            ("compiler", &["kernel", "stdlib"]),
        ]);
        let required = resolve(&shipment_apps, &erts_apps);
        assert!(validate(&required, &shipment_apps, &erts_apps).is_ok());
    }

    #[test]
    fn test_validate_fails_when_erts_app_missing() {
        let shipment_apps = apps(&[("my_app", &["my_lib"]), ("my_lib", &["crypto"])]);
        let erts_apps = apps(&[
            ("kernel", &[]),
            ("stdlib", &["kernel"]),
            ("compiler", &["kernel", "stdlib"]),
        ]);
        let required = resolve(&shipment_apps, &erts_apps);
        let err = validate(&required, &shipment_apps, &erts_apps).unwrap_err();
        assert_eq!(
            err.to_string(),
            "required OTP app(s) not found in ERTS: crypto. Try a different target or use --full-erts"
        );
    }

    #[test]
    fn test_resolve_always_includes_implicit_apps() {
        let shipment_apps = apps(&[("my_app", &[])]);
        let erts_apps = apps(&[
            ("kernel", &[]),
            ("stdlib", &["kernel"]),
            ("compiler", &["kernel", "stdlib"]),
        ]);

        let required = resolve(&shipment_apps, &erts_apps);
        assert!(required.contains("kernel"));
        assert!(required.contains("stdlib"));
        assert!(required.contains("compiler"));
        assert!(required.contains("my_app"));
    }

    #[quickcheck]
    fn test_resolve_closed_under_erts_deps(
        shipment_entries: Vec<(u8, Vec<u8>)>,
        erts_entries: Vec<(u8, Vec<u8>)>,
    ) -> bool {
        fn name(n: u8) -> String {
            format!("app{}", n % 8)
        }
        fn to_graph(entries: Vec<(u8, Vec<u8>)>) -> HashMap<String, Vec<String>> {
            entries
                .into_iter()
                .map(|(k, deps)| (name(k), deps.into_iter().map(name).collect()))
                .collect()
        }

        let shipment_apps = to_graph(shipment_entries);
        let erts_apps = to_graph(erts_entries);
        let required = resolve(&shipment_apps, &erts_apps);

        required.iter().all(|app| {
            erts_apps
                .get(app)
                .is_none_or(|deps| deps.iter().all(|d| required.contains(d)))
        })
    }
}
