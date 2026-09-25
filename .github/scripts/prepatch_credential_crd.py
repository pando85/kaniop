from pathlib import Path

p = Path("libs/person/src/crd.rs")
s = p.read_text()

if "pub enum CredentialState" not in s:
    marker = "/// Most recent observed status of the Kanidm Person Account. Read-only.\n"
    enum_def = """/// Observed credential state for a Kanidm person account.
///
/// `Unknown` is deliberately distinct from `Absent`: failures while reading credential
/// attributes must never be interpreted as permission to issue a credential update token.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = \"schemars\", derive(JsonSchema))]
#[serde(rename_all = \"camelCase\")]
pub enum CredentialState {
    Present,
    Absent,
    #[default]
    Unknown,
}

"""
    if marker not in s:
        raise SystemExit("missing credential state enum anchor")
    s = s.replace(marker, enum_def + marker, 1)

if "pub credential_state: CredentialState" not in s:
    marker = "    pub conditions: Option<Vec<Condition>>,\n"
    fields = """    pub conditions: Option<Vec<Condition>>,

    /// Last credential state observed through Kanidm's read-only person attributes.
    #[serde(default)]
    pub credential_state: CredentialState,

    /// Unix timestamp at which the currently issued bootstrap credential token expires.
    /// Persisting this in status prevents operator restarts from minting duplicate tokens.
    #[serde(default, skip_serializing_if = \"Option::is_none\")]
    pub credentials_token_expiry: Option<i64>,
"""
    if marker not in s:
        raise SystemExit("missing credential status fields anchor")
    s = s.replace(marker, fields, 1)

p.write_text(s)
