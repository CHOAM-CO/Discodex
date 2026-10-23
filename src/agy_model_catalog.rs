//! Normalizes Antigravity CLI variant rows for model and effort selection.
//!
//! The CLI currently exposes variants as model IDs and labels (for example,
//! `foo-high` / `Foo (High)`) and publishes its effort vocabulary separately.
//! Published variant IDs determine support; labels are display text only.

/// Codex orchestration mode; never a CLI-native AGY effort value.
pub(crate) const CODEX_ORCHESTRATION_EFFORT: &str = "ultra";

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Variant {
    pub id: String,
    pub label: String,
    pub effort: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Family {
    pub id: String,
    pub label: String,
    pub variants: Vec<Variant>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Invocation {
    pub model: String,
    /// The effort passed to AGY, resolved only from discovered metadata.
    pub effort: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct EffortAlias {
    pub requested: String,
    pub actual: String,
}

fn effort_order() -> &'static [String] {
    static ORDER: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
    ORDER.get_or_init(|| {
        serde_json::from_str(include_str!("../resources/agy_effort_order.json"))
            .expect("embedded AGY effort order is valid")
    })
}

fn floor_effort(family: &Family, requested: &str) -> Option<String> {
    let available = |effort: &str| {
        family
            .variants
            .iter()
            .find(|variant| variant.effort.eq_ignore_ascii_case(effort))
            .map(|variant| variant.effort.clone())
    };
    available(requested).or_else(|| {
        let order = effort_order();
        let index = order
            .iter()
            .position(|effort| effort.eq_ignore_ascii_case(requested))?;
        order[..index]
            .iter()
            .rev()
            .find_map(|effort| available(effort))
    })
}

/// Share the same supported-effort flooring between settings and execution.
/// UI/catalog serialization and CLI invocation share this metadata source.
pub(crate) fn effort_aliases(
    catalog: &crate::agy::Catalog,
    selected_model: &str,
) -> Vec<EffortAlias> {
    let Some(family) = families(catalog).into_iter().find(|family| {
        family.id == selected_model
            || family
                .variants
                .iter()
                .any(|variant| variant.id == selected_model)
    }) else {
        return Vec::new();
    };
    effort_aliases_for_family(&family)
}

pub(crate) fn effort_aliases_for_family(family: &Family) -> Vec<EffortAlias> {
    effort_order()
        .iter()
        .filter_map(|requested| {
            let actual = floor_effort(family, requested)?;
            (!actual.eq_ignore_ascii_case(requested)).then(|| EffortAlias {
                requested: requested.clone(),
                actual,
            })
        })
        .collect()
}

fn normalized_effort<'a>(value: &'a str, available: &'a [String]) -> Option<&'a str> {
    if value.eq_ignore_ascii_case(CODEX_ORCHESTRATION_EFFORT) {
        return None;
    }
    available
        .iter()
        .find(|effort| effort.eq_ignore_ascii_case(value))
        .map(String::as_str)
}

fn split_id<'a>(id: &'a str, efforts: &[String]) -> Option<(&'a str, &'a str)> {
    let (base, suffix) = id.rsplit_once('-')?;
    if base.is_empty() {
        return None;
    }
    normalized_effort(suffix, efforts)?;
    Some((base, suffix))
}

fn split_label<'a>(label: &'a str, efforts: &[String]) -> Option<(&'a str, &'a str)> {
    let open = label.rfind(" (")?;
    let suffix = label.get(open + 2..)?.strip_suffix(')')?;
    let family = label[..open].trim_end();
    if family.is_empty() {
        return None;
    }
    normalized_effort(suffix, efforts)?;
    Some((family, suffix))
}

/// Build family metadata exclusively from discovered model rows and effort
/// alternatives. Display-label formatting never determines capabilities.
pub(crate) fn families(catalog: &crate::agy::Catalog) -> Vec<Family> {
    let mut result: Vec<Family> = Vec::new();
    for model in &catalog.models {
        let Some((base_id, id_effort)) = split_id(&model.id, &catalog.efforts) else {
            continue;
        };
        let base_label = split_label(&model.label, &catalog.efforts)
            .map(|(label, _)| label)
            .unwrap_or(&model.label);
        let Some(effort) = normalized_effort(id_effort, &catalog.efforts) else {
            continue;
        };
        let index = result
            .iter()
            .position(|family| family.id == base_id)
            .unwrap_or_else(|| {
                result.push(Family {
                    id: base_id.to_owned(),
                    label: base_label.to_owned(),
                    variants: Vec::new(),
                });
                result.len() - 1
            });
        let family = &mut result[index];
        if !family.variants.iter().any(|variant| variant.id == model.id) {
            family.variants.push(Variant {
                id: model.id.clone(),
                label: model.label.clone(),
                effort: effort.to_owned(),
            });
        }
    }
    result
}

/// Project discovered IDs and model-specific efforts for the core's external catalog.
pub(crate) fn core_metadata(catalog: &crate::agy::Catalog) -> serde_json::Value {
    let mut rows = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut add =
        |model: &str, display_name: &str, efforts: &[String], multi_agent_effort: Option<&str>| {
            if seen.insert(model.to_owned()) {
                let mut row = serde_json::json!({
                        "model": model,
                        "display_name": display_name,
                        "supported_reasoning_efforts": efforts,
                });
                if let Some(effort) = multi_agent_effort {
                    row["multi_agent_reasoning_effort"] = serde_json::json!(effort);
                }
                rows.push(row);
            }
        };
    for family in families(catalog) {
        let mut efforts: Vec<String> = Vec::new();
        for variant in &family.variants {
            if !efforts
                .iter()
                .any(|effort| effort.eq_ignore_ascii_case(&variant.effort))
            {
                efforts.push(variant.effort.clone());
            }
        }
        let aliases = effort_aliases_for_family(&family);
        let multi_agent_effort = aliases
            .iter()
            .find(|alias| alias.requested == CODEX_ORCHESTRATION_EFFORT)
            .map(|alias| alias.actual.as_str());
        add(&family.id, &family.label, &efforts, multi_agent_effort);
        // Legacy variant selections use the same family and separate effort at invocation.
        for variant in &family.variants {
            add(&variant.id, &variant.label, &efforts, multi_agent_effort);
        }
    }
    for model in &catalog.models {
        add(&model.id, &model.label, &[], None);
    }
    serde_json::Value::Array(rows)
}

/// Separate model and effort, flooring only against published model variants.
/// Ultra stays in Codex instructions and resolves to the highest lower tier.
pub(crate) fn invocation(
    catalog: Option<&crate::agy::Catalog>,
    selected_model: &str,
    selected_effort: Option<&str>,
) -> Invocation {
    if let Some(catalog) = catalog {
        let families = families(catalog);
        if let Some(family) = families.iter().find(|family| {
            family.id == selected_model
                || family
                    .variants
                    .iter()
                    .any(|variant| variant.id == selected_model)
        }) {
            let requested = selected_effort.or_else(|| {
                family
                    .variants
                    .iter()
                    .find(|variant| variant.id == selected_model)
                    .map(|variant| variant.effort.as_str())
            });
            return Invocation {
                model: family.id.clone(),
                effort: requested.and_then(|effort| {
                    floor_effort(family, effort).or_else(|| {
                        (!effort.eq_ignore_ascii_case(CODEX_ORCHESTRATION_EFFORT))
                            .then(|| effort.to_owned())
                    })
                }),
            };
        }
    }

    Invocation {
        model: selected_model.to_owned(),
        effort: selected_effort
            .filter(|effort| !effort.eq_ignore_ascii_case(CODEX_ORCHESTRATION_EFFORT))
            .map(str::to_owned),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn catalog() -> crate::agy::Catalog {
        crate::agy::Catalog {
            models: vec![
                crate::agy::ModelOption {
                    id: "example-argon-high".into(),
                    label: "Example Argon (High)".into(),
                },
                crate::agy::ModelOption {
                    id: "example-argon-medium".into(),
                    label: "Example Argon (Medium)".into(),
                },
                crate::agy::ModelOption {
                    id: "example-argon-low".into(),
                    label: "Example Argon (Low)".into(),
                },
                crate::agy::ModelOption {
                    id: "example-flash-high".into(),
                    label: "Example Flash (High)".into(),
                },
                crate::agy::ModelOption {
                    id: "example-flash-medium".into(),
                    label: "Example Flash (Medium)".into(),
                },
                crate::agy::ModelOption {
                    id: "example-flash-low".into(),
                    label: "Example Flash (Low)".into(),
                },
                crate::agy::ModelOption {
                    id: "example-opus-low".into(),
                    label: "Example Opus (Low)".into(),
                },
                crate::agy::ModelOption {
                    id: "example-opus-medium".into(),
                    label: "Example Opus (Medium)".into(),
                },
                crate::agy::ModelOption {
                    id: "example-opus-high".into(),
                    label: "Example Opus (High)".into(),
                },
                crate::agy::ModelOption {
                    id: "example-sonnet-low".into(),
                    label: "Example Sonnet (Low)".into(),
                },
                crate::agy::ModelOption {
                    id: "example-sonnet-medium".into(),
                    label: "Example Sonnet (Medium)".into(),
                },
                crate::agy::ModelOption {
                    id: "example-sonnet-high".into(),
                    label: "Example Sonnet (High)".into(),
                },
                crate::agy::ModelOption {
                    id: "custom-variant".into(),
                    label: "Custom Variant".into(),
                },
            ],
            efforts: vec!["low".into(), "medium".into(), "high".into()],
        }
    }

    #[test]
    fn groups_only_observed_model_effort_pairs() {
        let groups = families(&catalog());
        assert_eq!(groups.len(), 4);
        assert_eq!(groups[0].id, "example-argon");
        assert_eq!(groups[0].label, "Example Argon");
        assert_eq!(
            groups[0]
                .variants
                .iter()
                .map(|variant| variant.effort.as_str())
                .collect::<Vec<_>>(),
            vec!["high", "medium", "low"]
        );
        assert!(groups.iter().all(|family| family.variants.len() == 3));
    }

    #[test]
    fn invocation_preserves_legacy_variant_and_resolves_explicit_aliases() {
        let catalog = catalog();
        assert_eq!(
            invocation(Some(&catalog), "example-argon", Some("medium")),
            Invocation {
                model: "example-argon".into(),
                effort: Some("medium".into())
            }
        );
        assert_eq!(
            invocation(Some(&catalog), "example-argon-low", None),
            Invocation {
                model: "example-argon".into(),
                effort: Some("low".into())
            }
        );
        assert_eq!(
            invocation(Some(&catalog), "example-opus", Some("xhigh")),
            Invocation {
                model: "example-opus".into(),
                effort: Some("high".into())
            }
        );
        assert_eq!(
            invocation(Some(&catalog), "custom-user-model", Some("xhigh")),
            Invocation {
                model: "custom-user-model".into(),
                effort: Some("xhigh".into())
            }
        );
    }

    #[test]
    fn ultra_is_preserved_for_orchestration_and_mapped_only_from_discovered_high() {
        let catalog = catalog();
        assert_eq!(
            invocation(Some(&catalog), "example-argon", Some("ultra")),
            Invocation {
                model: "example-argon".into(),
                effort: Some("high".into()),
            }
        );
        assert_eq!(
            invocation(Some(&catalog), "example-argon-high", Some("ultra"))
                .effort
                .as_deref(),
            Some("high")
        );
        assert_eq!(
            invocation(Some(&catalog), "custom-model", Some("ultra")).effort,
            None
        );
    }

    #[test]
    fn xhigh_and_ultra_use_the_highest_lower_discovered_effort() {
        let catalog = crate::agy::Catalog {
            models: vec![
                crate::agy::ModelOption {
                    id: "synthetic-model-low".into(),
                    label: "Synthetic Model (Low)".into(),
                },
                crate::agy::ModelOption {
                    id: "synthetic-model-medium".into(),
                    label: "Synthetic Model (Medium)".into(),
                },
            ],
            efforts: vec!["low".into(), "medium".into(), "high".into()],
        };
        assert_eq!(
            invocation(Some(&catalog), "synthetic-model", Some("xhigh")),
            Invocation {
                model: "synthetic-model".into(),
                effort: Some("medium".into()),
            }
        );
        assert_eq!(
            invocation(Some(&catalog), "synthetic-model", Some("ultra")),
            Invocation {
                model: "synthetic-model".into(),
                effort: Some("medium".into()),
            }
        );
    }

    #[test]
    fn selectable_effort_aliases_match_the_invocation_resolution() {
        let catalog = catalog();
        assert_eq!(
            effort_aliases(&catalog, "example-argon"),
            vec![
                EffortAlias {
                    requested: "xhigh".into(),
                    actual: "high".into(),
                },
                EffortAlias {
                    requested: "max".into(),
                    actual: "high".into(),
                },
                EffortAlias {
                    requested: "ultra".into(),
                    actual: "high".into(),
                },
            ]
        );
        for alias in effort_aliases(&catalog, "example-argon") {
            assert_eq!(
                invocation(Some(&catalog), "example-argon", Some(&alias.requested)).effort,
                Some(alias.actual)
            );
        }
        assert!(effort_aliases(&catalog, "custom-user-model").is_empty());
    }

    #[test]
    fn discovered_family_resolves_only_explicitly_supported_or_aliased_efforts() {
        let catalog = catalog();
        assert_eq!(
            invocation(Some(&catalog), "example-argon", Some("xhigh")),
            Invocation {
                model: "example-argon".into(),
                effort: Some("high".into()),
            }
        );
        assert_eq!(
            invocation(Some(&catalog), "example-argon", Some("HIGH")),
            Invocation {
                model: "example-argon".into(),
                effort: Some("high".into()),
            }
        );
        assert_eq!(
            invocation(Some(&catalog), "example-argon-low", Some("xhigh")),
            Invocation {
                model: "example-argon".into(),
                effort: Some("high".into()),
            }
        );
        assert_eq!(
            invocation(Some(&catalog), "custom-user-model", Some("xhigh")),
            Invocation {
                model: "custom-user-model".into(),
                effort: Some("xhigh".into()),
            }
        );
    }

    #[test]
    fn missing_catalog_preserves_exact_model_ids_and_only_explicit_effort() {
        assert_eq!(
            invocation(None, "foo-low", Some("high")),
            Invocation {
                model: "foo-low".into(),
                effort: Some("high".into())
            }
        );
        assert_eq!(
            invocation(None, "foo-xhigh", Some("ultra")),
            Invocation {
                model: "foo-xhigh".into(),
                effort: None
            }
        );
        assert_eq!(
            invocation(None, "foo-mystery", Some("custom-effort")),
            Invocation {
                model: "foo-mystery".into(),
                effort: Some("custom-effort".into())
            }
        );
        assert_eq!(
            invocation(None, "foo-low", None),
            Invocation {
                model: "foo-low".into(),
                effort: None
            }
        );
    }

    #[test]
    fn published_variants_do_not_depend_on_display_labels() {
        let catalog = crate::agy::Catalog {
            models: vec![crate::agy::ModelOption {
                id: "model-high".into(),
                label: "Model (Low)".into(),
            }],
            efforts: vec!["low".into(), "high".into()],
        };
        assert_eq!(families(&catalog).len(), 1);
        assert_eq!(
            invocation(Some(&catalog), "model-high", None),
            Invocation {
                model: "model".into(),
                effort: Some("high".into()),
            }
        );
    }

    #[test]
    fn published_variants_normalize_but_unknown_ids_are_preserved() {
        let mut catalog = catalog();
        catalog.models.push(crate::agy::ModelOption {
            id: "observed-low".into(),
            label: "Observed model".into(),
        });

        assert_eq!(
            invocation(Some(&catalog), "observed-low", None),
            Invocation {
                model: "observed".into(),
                effort: Some("low".into()),
            }
        );
        assert_eq!(
            invocation(Some(&catalog), "configured-high", Some("medium")),
            Invocation {
                model: "configured-high".into(),
                effort: Some("medium".into()),
            }
        );
    }
}
