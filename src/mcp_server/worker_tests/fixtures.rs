//! Exact integration-test environments, shared without a second policy model.
// These lexical wrappers expose private builders; the original integration tests
// remain available, while focused worker runs select the `worker_` test prefix.
mod ordinary {
    include!("../../../tests/engine.rs");
    pub(super) fn parity() {
        let (temp, result) = run("");
        super::super::compare_saved(
            result,
            load(&temp.path().join("theseus.toml")).unwrap(),
            temp.path(),
            || Fake { fault: "" },
        );
        let (temp, result) = run_reinstall("", "");
        super::super::compare_saved(
            result,
            load(&temp.path().join("theseus.toml")).unwrap(),
            temp.path(),
            ReinstallEnvironment::default,
        );
        let (temp, result, _) = run_interruption("");
        super::super::compare_saved(
            result,
            load(&temp.path().join("theseus.toml")).unwrap(),
            temp.path(),
            InterruptionEnvironment::default,
        );
        for rejection in [false, true] {
            let (temp, result, _) = run_downgrade(rejection, "");
            super::super::compare_saved(
                result,
                load(&temp.path().join("theseus.toml")).unwrap(),
                temp.path(),
                move || DowngradeEnvironment {
                    rejection,
                    ..Default::default()
                },
            );
        }
    }
}
mod rollback {
    include!("../../../tests/rollback_engine.rs");
    pub(super) fn parity() {
        for binary in [false, true] {
            for profile in [ProfileName::User, ProfileName::System] {
                let (temp, result, _) = run("", binary, profile, None);
                super::super::compare_saved(
                    result,
                    recipe::load(&temp.path().join("theseus.toml")).unwrap(),
                    temp.path(),
                    || Evidence::new(""),
                );
            }
        }
    }
}
mod interrupted_rollback {
    include!("../../../tests/interrupted_rollback_engine.rs");
    pub(super) fn parity() {
        for binary in [false, true] {
            for profile in [ProfileName::User, ProfileName::System] {
                let (temp, result, _) = run("", binary, profile, None);
                super::super::compare_saved(
                    result,
                    recipe::load(&temp.path().join("theseus.toml")).unwrap(),
                    temp.path(),
                    || Evidence::new(""),
                );
            }
        }
    }
}
mod plugin {
    include!("../../../tests/plugin_engine.rs");
    pub(super) fn parity() {
        for scope in [PluginScope::User, PluginScope::Project, PluginScope::Local] {
            let temp = fixture::project();
            let recipe = recipe::parse_bytes(
                &temp.path().canonicalize().unwrap().join("recipe.toml"),
                &fixture::bytes(&fixture::declaration(scope.as_str())),
            )
            .unwrap();
            let make = move || ReplayEnvironment {
                scope,
                fault: String::new(),
                calls: vec![],
                created: 0,
                setups: 0,
                removed: 0,
                stopped: 0,
                observations: 0,
                phase: String::new(),
            };
            let options = RunOptions {
                output_root: temp.path().join("direct"),
                ..RunOptions::all(&recipe)
            };
            let direct = engine::run_with(
                &recipe,
                options,
                CancellationToken::default(),
                &mut make(),
                |_| {},
            )
            .unwrap();
            super::super::compare_saved(direct, recipe, temp.path(), make);
        }
    }
}
#[path = "../../../tests/support/mcp.rs"]
mod guest_mcp;

pub(super) fn ordinary_parity() {
    ordinary::parity();
}
pub(super) fn rollback_parity() {
    rollback::parity();
}
pub(super) fn interrupted_rollback_parity() {
    interrupted_rollback::parity();
}
pub(super) fn plugin_parity() {
    plugin::parity();
}
pub(super) fn guest_mcp_parity() {
    for mode in ["", "upgrade:", "plugin:"] {
        let fixture = guest_mcp::Fixture::new();
        let declaration = match mode {
            "upgrade:" => guest_mcp::upgrade_declaration(),
            "plugin:" => guest_mcp::plugin_declaration(),
            _ => guest_mcp::declaration(),
        };
        let recipe = fixture.validate(&declaration).unwrap();
        let options = crate::engine::RunOptions {
            output_root: fixture.root.path().join("direct"),
            ..crate::engine::RunOptions::all(&recipe)
        };
        let direct = crate::engine::run_with(
            &recipe,
            options,
            crate::engine::CancellationToken::default(),
            &mut guest_mcp::environment(mode),
            |_| {},
        )
        .unwrap();
        super::compare_saved(direct, recipe, fixture.root.path(), move || {
            guest_mcp::environment(mode)
        });
    }
}
