use dprint_cli_core::checksums::ChecksumPathOrUrl;
use serde::{Deserialize, Serialize};
use std::cmp::{Ord, Ordering, PartialOrd};
use std::collections::hash_map::Values;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use crate::environment::{Environment, PATH_SEPARATOR};
use crate::plugins::{get_plugin_dir_relative_local_user_data, BinaryEnvironment};
use crate::types::{BinaryName, CommandName, NameSelector, Version, VersionSelector};

const PATH_GLOBAL_VERSION_VALUE: &'static str = "path";
const IDENTIFIER_GLOBAL_PREFIX: &'static str = "identifier:";

#[derive(Clone, Serialize, Deserialize, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PluginsManifest {
    // Key is url.
    pub(super) urls_to_identifier: HashMap<String, BinaryIdentifier>,
    pub(super) global_versions: GlobalVersionsMap,
    pub(super) binaries: HashMap<BinaryIdentifier, BinaryManifestItem>,
    /// Changes to the environment that need to be made.
    pub(super) pending_env_changes: PendingEnvironmentChanges,
    /// Current binary paths.
    pub(super) binary_paths: Vec<String>,
}

#[derive(Clone, Serialize, Deserialize, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(super) struct PendingEnvironmentChanges {
    added: HashSet<BinaryIdentifier>,
    removed: HashSet<BinaryIdentifier>,
}

impl PendingEnvironmentChanges {
    pub fn mark_for_adding(&mut self, identifier: BinaryIdentifier) {
        // always remove and always insert so its more reliable
        self.removed.remove(&identifier);
        self.added.insert(identifier.clone());
    }

    pub fn mark_for_removal(&mut self, identifier: BinaryIdentifier) {
        self.added.remove(&identifier);
        self.removed.insert(identifier);
    }

    pub fn any(&self) -> bool {
        !self.added.is_empty() || !self.removed.is_empty()
    }

    pub fn clear(&mut self) {
        self.added.clear();
        self.removed.clear();
    }
}

#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct BinaryManifestItemSource {
    pub path: String,
    pub checksum: String,
}

#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct BinaryManifestItem {
    pub name: BinaryName,
    pub version: Version,
    /// Created time in *seconds* since epoch.
    pub created_time: u64,
    pub commands: Vec<BinaryManifestItemCommand>,
    // Source for reinstalling.
    pub source: BinaryManifestItemSource,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub environment: Option<BinaryEnvironment>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub on_use: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub on_stop_use: Option<String>,
}

impl BinaryManifestItem {
    pub fn get_identifier(&self) -> BinaryIdentifier {
        BinaryIdentifier::new(&self.name, &self.version)
    }

    pub fn matches(&self, name_selector: &NameSelector) -> bool {
        name_selector.is_match(&self.name)
    }

    pub fn get_command_names(&self) -> Vec<CommandName> {
        // todo: return iterator somehow?
        self.commands.iter().map(|c| c.name.clone()).collect()
    }

    pub fn get_env_paths(&self) -> Vec<String> {
        self.environment
            .as_ref()
            .and_then(|e| e.paths.as_ref())
            .map(|p| p.clone())
            .unwrap_or(Vec::new())
    }
}

impl PartialOrd for BinaryManifestItem {
    fn partial_cmp(&self, other: &BinaryManifestItem) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for BinaryManifestItem {
    fn cmp(&self, other: &BinaryManifestItem) -> Ordering {
        let name_ordering = self.name.cmp(&other.name);
        match name_ordering {
            Ordering::Equal => self.version.partial_cmp(&other.version).unwrap(),
            _ => name_ordering,
        }
    }
}

#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct BinaryManifestItemCommand {
    pub name: CommandName,
    /// The relative path to the file name.
    pub path: String,
}

#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Eq, Hash)]
#[serde(rename_all = "camelCase")]
pub struct BinaryIdentifier(String);

impl BinaryIdentifier {
    pub fn new(name: &BinaryName, version: &Version) -> Self {
        BinaryIdentifier(format!("{}||{}||{}", name.owner, name.name.as_str(), version))
    }

    pub fn get_binary_name(&self) -> BinaryName {
        let parts = self.0.split("||").collect::<Vec<_>>();
        BinaryName::new(parts[0].to_string(), parts[1].to_string())
    }

    pub fn get_version(&self) -> Version {
        let parts = self.0.split("||").collect::<Vec<_>>();
        Version::parse(parts[2]).unwrap()
    }
}

#[derive(Clone)]
pub enum GlobalBinaryLocation {
    /// Use a bvm binary.
    Bvm(BinaryIdentifier),
    /// Use the binary on the path.
    Path,
}

impl GlobalBinaryLocation {
    pub fn to_identifier_option(&self) -> Option<BinaryIdentifier> {
        if let GlobalBinaryLocation::Bvm(identifier) = self {
            Some(identifier.clone())
        } else {
            None
        }
    }
}

impl From<BinaryIdentifier> for GlobalBinaryLocation {
    fn from(identifier: BinaryIdentifier) -> Self {
        GlobalBinaryLocation::Bvm(identifier)
    }
}

#[derive(Clone, Serialize, Deserialize, Debug, PartialEq)]
pub(super) struct GlobalVersionsMap(HashMap<String, String>);

impl GlobalVersionsMap {
    pub(super) fn set(&mut self, command_name: CommandName, location: GlobalBinaryLocation) {
        self.0.insert(
            command_name.into_string(),
            match location {
                GlobalBinaryLocation::Path => PATH_GLOBAL_VERSION_VALUE.to_string(),
                GlobalBinaryLocation::Bvm(identifier) => format!("{}{}", IDENTIFIER_GLOBAL_PREFIX, identifier.0),
            },
        );
    }

    pub(super) fn get(&self, command_name: &CommandName) -> Option<GlobalBinaryLocation> {
        self.0.get(command_name.as_str()).map(|value| {
            if value == PATH_GLOBAL_VERSION_VALUE {
                GlobalBinaryLocation::Path
            } else if value.starts_with(IDENTIFIER_GLOBAL_PREFIX) {
                GlobalBinaryLocation::Bvm(BinaryIdentifier(value[IDENTIFIER_GLOBAL_PREFIX.len()..].to_string()))
            } else {
                // todo: don't panic and improve this
                panic!("Unknown value: {}", value);
            }
        })
    }

    pub(super) fn remove(&mut self, command_name: &CommandName) {
        self.0.remove(command_name.as_str());
    }
}

impl PluginsManifest {
    fn new() -> PluginsManifest {
        PluginsManifest {
            global_versions: GlobalVersionsMap(HashMap::new()),
            binaries: HashMap::new(),
            urls_to_identifier: HashMap::new(),
            pending_env_changes: PendingEnvironmentChanges {
                added: HashSet::new(),
                removed: HashSet::new(),
            },
            binary_paths: Vec::new(),
        }
    }

    pub fn load<TEnvironment: Environment>(environment: &TEnvironment) -> PluginsManifest {
        // If a system wide lock is ever added here, remember that some
        // people might run "bvm util is-installed owner/name" while this may
        // have a lock. In that case, only have a lock on writing, but not
        // on reading.
        let file_path = get_manifest_file_path(environment);
        match environment.read_file_text(&file_path) {
            Ok(text) => match serde_json::from_str(&text) {
                Ok(manifest) => manifest,
                Err(err) => {
                    environment.log_error(&format!("Error deserializing plugins manifest: {}", err));
                    PluginsManifest::new()
                }
            },
            Err(_) => PluginsManifest::new(),
        }
    }

    // url to identifier

    pub fn get_identifier_from_url(&self, url: &ChecksumPathOrUrl) -> Option<&BinaryIdentifier> {
        self.urls_to_identifier.get(&url.path_or_url)
    }

    // pending environment changes

    pub fn get_relative_pending_added_paths(&self) -> Vec<String> {
        self.get_change_paths(self.pending_env_changes.added.iter())
    }

    pub fn get_relative_pending_removed_paths(&self) -> Vec<String> {
        self.get_change_paths(self.pending_env_changes.removed.iter())
    }

    fn get_change_paths<'a>(&self, changes: impl Iterator<Item = &'a BinaryIdentifier>) -> Vec<String> {
        let mut result = Vec::new();
        for identifier in changes {
            result.extend(self.get_binary_env_paths(&identifier));
        }
        result
    }

    fn get_binary_env_paths(&self, identifier: &BinaryIdentifier) -> Vec<String> {
        if let Some(binary) = self.get_binary(&identifier) {
            let bin_dir = get_plugin_dir_relative_local_user_data(&binary.name, &binary.version);
            binary.get_env_paths()
                .into_iter()
                .map(|path| format!("{}{}{}", bin_dir.to_string_lossy(), PATH_SEPARATOR, path))
                .collect()
        } else {
            Vec::new()
        }
    }

    // binary environment paths

    pub fn get_bin_env_paths(&self) -> &Vec<String> {
        &self.binary_paths
    }

    // binary

    pub fn get_binary(&self, identifier: &BinaryIdentifier) -> Option<&BinaryManifestItem> {
        self.binaries.get(identifier)
    }

    pub fn has_binary(&self, identifier: &BinaryIdentifier) -> bool {
        self.get_binary(identifier).is_some()
    }

    pub fn binaries(&self) -> Values<'_, BinaryIdentifier, BinaryManifestItem> {
        self.binaries.values()
    }

    pub fn has_binary_with_command(&self, name: &CommandName) -> bool {
        self.binaries().any(|b| b.commands.iter().any(|c| &c.name == name))
    }

    pub fn binary_name_has_same_owner(&self, binary_name: &BinaryName) -> bool {
        let binaries = self
            .binaries()
            .filter(|b| b.name.name == binary_name.name)
            .collect::<Vec<_>>();
        if let Some(first_binary) = binaries.get(0) {
            let first_owner = &first_binary.name.owner;
            binaries.iter().all(|b| &b.name.owner == first_owner)
        } else {
            true
        }
    }

    pub fn get_binaries_matching_name(&self, name_selector: &NameSelector) -> Vec<&BinaryManifestItem> {
        self.binaries().filter(|b| b.matches(name_selector)).collect()
    }

    pub fn get_binaries_matching_name_and_version(
        &self,
        name_selector: &NameSelector,
        version: &VersionSelector,
    ) -> Vec<&BinaryManifestItem> {
        self.binaries()
            .filter(|b| b.matches(name_selector) && version.matches(&b.version))
            .collect()
    }

    pub fn get_binaries_with_command(&self, name: &CommandName) -> Vec<&BinaryManifestItem> {
        self.binaries()
            .filter(|b| b.commands.iter().any(|c| &c.name == name))
            .collect()
    }

    pub fn get_latest_binary_with_name(&self, name: &BinaryName) -> Option<&BinaryManifestItem> {
        let mut binaries = self.binaries().filter(|b| &b.name == name).collect::<Vec<_>>();
        binaries.sort();
        binaries.pop()
    }

    pub fn get_latest_binary_with_command(&self, name: &CommandName) -> Option<&BinaryManifestItem> {
        let mut binaries = self.get_binaries_with_command(name);
        binaries.sort();
        binaries.pop()
    }

    pub fn get_global_binary_location(&self, command_name: &CommandName) -> Option<GlobalBinaryLocation> {
        self.global_versions.get(command_name)
    }

    pub fn is_global_version(&self, identifier: &BinaryIdentifier, command_name: &CommandName) -> bool {
        if let Some(GlobalBinaryLocation::Bvm(global_version_identifier)) = self.global_versions.get(command_name) {
            &global_version_identifier == identifier
        } else {
            false
        }
    }

    pub fn has_any_global_command(&self, identifier: &BinaryIdentifier) -> bool {
        if let Some(binary) = self.get_binary(&identifier) {
            for command in binary.commands.iter() {
                if self.is_global_version(identifier, &command.name) {
                    return true;
                }
            }
        }

        false
    }

    pub fn get_environment_paths(&self, identifier: &BinaryIdentifier) -> Vec<String> {
        self.get_binary(identifier)
            .map(|b| b.get_env_paths())
            .unwrap_or(Vec::new())
    }

    pub fn has_environment_paths(&self, identifier: &BinaryIdentifier) -> bool {
        !self.get_environment_paths(identifier).is_empty()
    }

    pub fn get_global_command_names(&self, identifier: &BinaryIdentifier) -> Vec<CommandName> {
        let mut result = Vec::new();
        if let Some(item) = self.binaries.get(identifier) {
            for command_name in item.get_command_names() {
                if self.is_global_version(identifier, &command_name) {
                    result.push(command_name);
                }
            }
        }
        result
    }
}

pub(super) fn get_manifest_file_path(environment: &impl Environment) -> PathBuf {
    let user_data_dir = environment.get_user_data_dir(); // share across domains
    user_data_dir.join("binaries-manifest.json")
}
