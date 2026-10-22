use serde::Deserialize;
use std::sync::LazyLock;

const PRODUCT_RESOURCE: &str = include_str!("../../resources/product.json");

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub(super) enum PlatformOs {
    Linux,
    Macos,
    Windows,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ProductMetadata {
    pub name: String,
    pub service_name: String,
    pub legacy_app_name: String,
    pub legacy_service_name: String,
    pub repository: String,
    pub github_api_root: String,
    pub release_download_root: String,
    pub version_tag_prefix: String,
    pub data_directory: String,
    pub codex_home_directory: String,
    pub runtime_directory: String,
    pub updates_file: String,
    pub updates_directory: String,
    pub prepared_update_directory: String,
    pub separate_codex_home_directory: String,
    pub migration_marker_file: String,
    pub official_runtime_directory: String,
    pub config_file: String,
    pub ownership_file: String,
    pub installation_list_file: String,
    pub credentials_file: String,
    pub state_directory: String,
    pub binary_directory: String,
    pub message_access_file: String,
    pub discord_command_file: String,
    pub runtime_manifest_file: String,
    pub runtime_manifest_format_version: u32,
    pub runtime_manifest_asset_template: String,
    pub runtime_archive_template: String,
    pub worker_asset_template: String,
    pub default_update_check_interval_seconds: u64,
    pub install_temp_template: String,
    pub update_temp_template: String,
    pub install_staging_template: String,
    pub supported_languages: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RuntimeMetadata {
    pub common_files: Vec<String>,
    pub platform_files: std::collections::BTreeMap<String, Vec<String>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct PlatformMetadata {
    pub os: PlatformOs,
    pub arch: String,
    pub target: String,
    pub extension: String,
    pub shell_os: Option<String>,
    pub shell_arch: Vec<String>,
    pub powershell_arch: Vec<String>,
    pub release_runner: String,
    pub test_runner: Option<String>,
    pub test: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ResourceFile {
    product: ProductMetadata,
    runtime: RuntimeMetadata,
    platforms: Vec<PlatformMetadata>,
}

#[derive(Debug)]
pub(super) struct ProductResources {
    pub product: ProductMetadata,
    pub runtime: RuntimeMetadata,
    pub platforms: Vec<PlatformMetadata>,
}

static RESOURCES: LazyLock<ProductResources> = LazyLock::new(|| {
    let resource: ResourceFile = serde_json::from_str(PRODUCT_RESOURCE)
        .expect("resources/product.json must match its typed schema");
    ProductResources {
        product: resource.product,
        runtime: resource.runtime,
        platforms: resource.platforms,
    }
});

pub(super) fn resources() -> &'static ProductResources {
    &RESOURCES
}

pub(super) fn platform(os: &str, arch: &str) -> Option<&'static PlatformMetadata> {
    resources()
        .platforms
        .iter()
        .find(|platform| platform.os.as_str() == os && platform.arch == arch)
}

pub(super) fn platform_for_target(target: &str) -> Option<&'static PlatformMetadata> {
    resources()
        .platforms
        .iter()
        .find(|platform| platform.target == target)
}

impl PlatformOs {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Linux => "linux",
            Self::Macos => "macos",
            Self::Windows => "windows",
        }
    }
}
