//! Normalizes Antigravity CLI variant rows for model and effort selection.
//!
//! The CLI currently exposes variants as model IDs and labels (for example,
//! `foo-high` / `Foo (High)`) and publishes its effort vocabulary separately.
//! We only form a family when both observed fields agree on a discovered effort.

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

#[derive(serde::Deserialize)]
struct EffortAliasRule {
    requested: String,
    actual: String,
    requires: Vec<String>,
    excludes: Vec<String>,
}

fn effort_alias_rules() -> &'static [EffortAliasRule] {
    static RULES: std::sync::OnceLock<Vec<EffortAliasRule>> = std::sync::OnceLock::new();
    RULES.get_or_init(|| {
        serde_json::from_str(include_str!("../resources/agy_effort_aliases.json"))
            .expect("embedded AGY effort aliases are valid")
    })
}

fn resolve_alias_for_family(family: &Family, requested: &str) -> Option<String> {
    let has_effort = |effort: &str| {
        family
            .variants
            .iter()
            .any(|variant| variant.effort.eq_ignore_ascii_case(effort))
    };
    effort_alias_rules()
        .iter()
        .find(|rule| rule.requested.eq_ignore_ascii_case(requested))
        .filter(|rule| rule.requires.iter().all(|effort| has_effort(effort)))
        .filter(|rule| rule.excludes.iter().all(|effort| !has_effort(effort)))
        .and_then(|rule| {
            family
                .variants
                .iter()
                .find(|variant| variant.effort.eq_ignore_ascii_case(&rule.actual))
                .map(|variant| variant.effort.clone())
        })
}

/// Return the explicitly configured aliases that apply to a discovered model.
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
    effort_alias_rules()
        .iter()
        .filter_map(|rule| {
            resolve_alias_for_family(&family, &rule.requested).map(|actual| EffortAlias {
                requested: rule.requested.clone(),
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
/// alternatives. A row becomes a family variant only when slug and label agree.
pub(crate) fn families(catalog: &crate::agy::Catalog) -> Vec<Family> {
    let mut result: Vec<Family> = Vec::new();
    for model in &catalog.models {
        let (Some((base_id, id_effort)), Some((base_label, label_effort))) = (
            split_id(&model.id, &catalog.efforts),
            split_label(&model.label, &catalog.efforts),
        ) else {
            continue;
        };
        if !id_effort.eq_ignore_ascii_case(label_effort) {
            continue;
        }
        let Some(effort) = normalized_effort(id_effort, &catalog.efforts) else {
            continue;
        };
        let index = result
            .iter()
            .position(|family| family.id == base_id && family.label == base_label)
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

/// Resolve only metadata-confirmed variants into a base alias and CLI effort.
/// Without a matching discovered family, preserve the exact model ID. `ultra`
/// is a Codex orchestration marker and is never sent to AGY; a legacy variant
/// retains its concrete effort only when discovery metadata identifies it.
pub(crate) fn invocation(
    catalog: Option<&crate::agy::Catalog>,
    selected_model: &str,
    selected_effort: Option<&str>,
) -> Invocation {
    let requested =
        selected_effort.filter(|effort| !effort.eq_ignore_ascii_case(CODEX_ORCHESTRATION_EFFORT));
    if let Some(catalog) = catalog {
        let families = families(catalog);
        if let Some(family) = families.iter().find(|family| {
            family.id == selected_model
                || family
                    .variants
                    .iter()
                    .any(|variant| variant.id == selected_model)
        }) {
            let legacy_effort = family
                .variants
                .iter()
                .find(|variant| variant.id == selected_model)
                .map(|variant| variant.effort.as_str());
            let compatible_requested = requested.and_then(|effort| {
                family
                    .variants
                    .iter()
                    .find(|variant| variant.effort.eq_ignore_ascii_case(effort))
                    .map(|variant| variant.effort.as_str())
            });
            let mapped =
                selected_effort.and_then(|effort| resolve_alias_for_family(family, effort));
            return Invocation {
                model: family.id.clone(),
                effort: compatible_requested
                    .map(str::to_owned)
                    .or(mapped)
                    .or_else(|| legacy_effort.map(str::to_owned)),
            };
        }

        // A present discovery snapshot is authoritative. If this exact ID was
        // not safely grouped from observed ID/label metadata, do not infer a
        // base alias from its spelling. This also preserves explicitly
        // configured IDs absent from the discovery snapshot.
        return Invocation {
            model: selected_model.to_owned(),
            effort: requested.map(str::to_owned),
        };
    }

    Invocation {
        model: selected_model.to_owned(),
        effort: requested.map(str::to_owned),
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
    fn xhigh_and_ultra_do_not_map_without_discovered_high_support() {
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
                effort: None,
            }
        );
        assert_eq!(
            invocation(Some(&catalog), "synthetic-model", Some("ultra")),
            Invocation {
                model: "synthetic-model".into(),
                effort: None,
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
    fn mismatched_slug_and_label_are_not_normalized() {
        let catalog = crate::agy::Catalog {
            models: vec![crate::agy::ModelOption {
                id: "model-high".into(),
                label: "Model (Low)".into(),
            }],
            efforts: vec!["low".into(), "high".into()],
        };
        assert!(families(&catalog).is_empty());
        assert_eq!(
            invocation(Some(&catalog), "model-high", None),
            Invocation {
                model: "model-high".into(),
                effort: None,
            }
        );
    }

    #[test]
    fn present_catalog_does_not_infer_suffix_for_unobserved_or_unmatched_id() {
        let mut catalog = catalog();
        catalog.models.push(crate::agy::ModelOption {
            id: "observed-low".into(),
            label: "Observed model".into(),
        });

        assert_eq!(
            invocation(Some(&catalog), "observed-low", None),
            Invocation {
                model: "observed-low".into(),
                effort: None,
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
