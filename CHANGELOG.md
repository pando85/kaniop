# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/), and this project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [v0.4.1](https://github.com/pando85/kaniop/tree/v0.4.1) - 2026-01-20

### Fixed

- Allow usage of non 'prometheus' named data sources in grafana ([cca7ad7](https://github.com/pando85/kaniop/commit/cca7ad78328d030a4792a5e6a9050297d845ed52))

### Build

- deps: Update ghcr.io/rash-sh/rash Docker tag to v2.17.6 ([e2cbd5a](https://github.com/pando85/kaniop/commit/e2cbd5aaaa08f18d30677992f4ddb5b4af334866))
- deps: Update Rust crate thiserror to v2.0.18 ([10a23a5](https://github.com/pando85/kaniop/commit/10a23a59c92c82e13c19de239d52039f11db3ba8))
- deps: Update ghcr.io/rash-sh/rash Docker tag to v2.17.7 ([bb0aa0f](https://github.com/pando85/kaniop/commit/bb0aa0f096652e2c729546f5016d94498acb448b))

## [v0.4.0](https://github.com/pando85/kaniop/tree/v0.4.0) - 2026-01-16

### Added

- group: Add account policy support for KanidmGroup ([87f8646](https://github.com/pando85/kaniop/commit/87f86463ee5cc3a90529d2f69f61991eccc71eaf))

### Fixed

- group: Reset account policy attributes when spec value is None ([6c0865f](https://github.com/pando85/kaniop/commit/6c0865ff3d26f587b540079f89dbaf834054d188))

### Build

- deps: Update Rust crate url to v2.5.8 ([2f9ca47](https://github.com/pando85/kaniop/commit/2f9ca47a6bc7754981cc4e70b224fba61ca04a0b))
- deps: Update Rust crate serde_json to v1.0.149 ([44f9aef](https://github.com/pando85/kaniop/commit/44f9aefa54ee94057fce8de86c0cba529b598405))
- deps: Update Rust crate rustls to v0.23.36 ([834bcc0](https://github.com/pando85/kaniop/commit/834bcc036ebf8e57ec3dadd18bd3fec2872761e5))
- deps: Update pre-commit hook alessandrojcm/commitlint-pre-commit-hook to v9.24.0 ([c7a612c](https://github.com/pando85/kaniop/commit/c7a612c58158ed6282a94f0b388efda26065af90))
- deps: Update pre-commit hook adrienverge/yamllint to v1.38.0 ([f30d8bb](https://github.com/pando85/kaniop/commit/f30d8bb955fcd341c5d98575c259ef5ab36f0241))
- deps: Update Rust crate kube to v3 ([04a9aae](https://github.com/pando85/kaniop/commit/04a9aae268e7c94bfa84275daeadd858b163877b))
- deps: Update Rust crate chrono to v0.4.43 ([a072cd5](https://github.com/pando85/kaniop/commit/a072cd54e98b075cd898010b35a557ee847e92b0))

### Refactor

- Replace chrono with jiff ([2e149bc](https://github.com/pando85/kaniop/commit/2e149bc36300e01cf1c4e6533289ae357c55d8ae))

### Testing

- ci: Update kind to 1.33.7 ([04082b5](https://github.com/pando85/kaniop/commit/04082b55669d61a9cabd1853c92fcd42aa44af0e))

## [v0.3.2](https://github.com/pando85/kaniop/tree/v0.3.2) - 2026-01-05

### Build

- ci: Split publish task to new step ([2ceb962](https://github.com/pando85/kaniop/commit/2ceb9629b51fc3be758df7ef149572e4b85ad175))

## [v0.3.1](https://github.com/pando85/kaniop/tree/v0.3.1) - 2026-01-05

### Build

- ci: Free some space before publish ([08f7aa7](https://github.com/pando85/kaniop/commit/08f7aa71500c767e3c94108081526f39a4cf1c23))

## [v0.3.0](https://github.com/pando85/kaniop/tree/v0.3.0) - 2026-01-05

### Added

- kanidm: Add statefulSetAnnotations-in-replica-groups ([43d25ca](https://github.com/pando85/kaniop/commit/43d25cab5e5d4590519024ba8dde7bd73bd5a642))

### Fixed

- kanidm: Server role write replica no ui had a typo ([327febd](https://github.com/pando85/kaniop/commit/327febd6e9b7b6fd3220b623c8b4f747fcd1c2e5))
- person: Generate URL token link using origin if defined ([71e4794](https://github.com/pando85/kaniop/commit/71e47949a59c27d4ec40ee0a2a42f271fd275646))

### Build

- deps: Update Rust crate tokio-util to v0.7.18 ([3f88f52](https://github.com/pando85/kaniop/commit/3f88f52ac0462317f5c24ef0b9754d0ba21c90ff))
- deps: Update Rust crate tokio to v1.49.0 ([61947e7](https://github.com/pando85/kaniop/commit/61947e74e11cbb249ffe3355308464eb68ecd312))

## [v0.2.0](https://github.com/pando85/kaniop/tree/v0.2.0) - 2026-01-02

### Added

- oauth2: Add automatic secret rotation for OAuth2 client secrets ([9cd936c](https://github.com/pando85/kaniop/commit/9cd936c48bb08639eed77b8a4991dd245206823d))
- sa: Add automatic secret rotation for passwords and API tokens ([3e54c5a](https://github.com/pando85/kaniop/commit/3e54c5ae976a20a7c94d6ee16a72eb67ca9fb9d3))

### Fixed

- chart: Rename prometheus rule and dashboard label to status ([28968e2](https://github.com/pando85/kaniop/commit/28968e2e3e6a679c70d522c7041fd7674d22c422))

### Build

- deps: Update ghcr.io/rash-sh/rash Docker tag to v2.17.5 ([68cf566](https://github.com/pando85/kaniop/commit/68cf5661401bda8d8b056bfcd15bdfd113f4b38b))
- deps: Update Rust crate axum-server to 0.8 ([d2ba59e](https://github.com/pando85/kaniop/commit/d2ba59e29892d395af736cd3ad878e5dcb9ab759))
- deps: Update Rust crate schemars to v1.2.0 ([8fde75f](https://github.com/pando85/kaniop/commit/8fde75f8ad3d7b76ecb3a03ebbe25753ecbf100d))
- deps: Update Rust crate serde_json to v1.0.148 ([c1d0206](https://github.com/pando85/kaniop/commit/c1d0206c2d00fb02c0881d2096e6b7a4f9721acd))
- deps: Update pre-commit hook renovatebot/pre-commit-hooks to v41.173.1 ([b0a6f16](https://github.com/pando85/kaniop/commit/b0a6f16dd24c60136a4ed4a1b8d0720c1ed0f276))
- deps: Update pre-commit hook renovatebot/pre-commit-hooks to v42 ([e711c8a](https://github.com/pando85/kaniop/commit/e711c8a32662edfc75e9312c27ffbecb3e7c6b1d))

### Testing

- oauth2: Add e2e tests for secret rotation ([c6ab2b6](https://github.com/pando85/kaniop/commit/c6ab2b62fb30c67d8f4e49b8f18726eabfdaef7a))
- sa: Add e2e tests for secret rotation ([253b489](https://github.com/pando85/kaniop/commit/253b4890f7e7fa452c94edfab7d692cd9a8bf130))

## [v0.1.10](https://github.com/pando85/kaniop/tree/v0.1.10) - 2025-12-25

### Added

- Disallow upgrade multiple minors at once ([331da0f](https://github.com/pando85/kaniop/commit/331da0f197150111d6c5cb8fd719a6707f114159))

### Fixed

- sa: Implement mail attribute handle ([1aff41b](https://github.com/pando85/kaniop/commit/1aff41b0e1cee4917c5654181ae1a9d3eedb2a48))
- Ensure CHANGELOG commit IDs are correct on release process ([925336e](https://github.com/pando85/kaniop/commit/925336ebd88f0de04e8e8ccd55ecce5c2764a4db))

### Build

- deps: Update Rust crate testcontainers to 0.26 ([ec1ebac](https://github.com/pando85/kaniop/commit/ec1ebac3c47dbb238e2b4ffd43ad0ad4fe7e457e))
- deps: Update Rust crate k8s-openapi to v0.26.1 ([778c496](https://github.com/pando85/kaniop/commit/778c49627e7a3b6b0640922ab8d207144d1b4bbb))
- deps: Update Rust crate hyper-util to v0.1.19 ([3685019](https://github.com/pando85/kaniop/commit/3685019144ea044b2023e184109381ccc182a6f5))
- deps: Update Rust crate uuid to v1.19.0 ([ef9ab5f](https://github.com/pando85/kaniop/commit/ef9ab5f7462bd27eb42947a81f4d49d7f162b267))
- deps: Update actions/cache action to v5 ([11090c7](https://github.com/pando85/kaniop/commit/11090c7800326836367021c57b4401c4184b2dc8))
- deps: Update kanidm to v1.8.5 ([343e840](https://github.com/pando85/kaniop/commit/343e840b8c7a532539c71536808f5c5f8c6c4541))
- deps: Update Rust crate tracing to v0.1.44 ([a098c14](https://github.com/pando85/kaniop/commit/a098c147ce3cfd4069ee5a312eab305cfa2cf124))
- deps: Update Rust crate testcontainers to v0.26.2 ([9447eb5](https://github.com/pando85/kaniop/commit/9447eb5932032d2e94999f8afe8fbf5717ff18fb))
- deps: Update Rust crate axum to v0.8.8 ([b8774b3](https://github.com/pando85/kaniop/commit/b8774b31a3855853471f3c311dab55aca9b8ede9))
- deps: Update dependency kubernetes-sigs/kind to v0.31.0 ([9aeadf8](https://github.com/pando85/kaniop/commit/9aeadf8764d33a591b621293315ec3ccf256334b))
- deps: Update Rust crate serde_json to v1.0.146 ([260e055](https://github.com/pando85/kaniop/commit/260e0552fd95aeb1ddcc68275879da2554f1da7a))
- deps: Update Rust crate tempfile to v3.24.0 ([f1c6e08](https://github.com/pando85/kaniop/commit/f1c6e0850187f6b03488df7433895e4389d9a4ce))
- deps: Update Rust crate serde_json to v1.0.147 ([b2743b1](https://github.com/pando85/kaniop/commit/b2743b1b06dfff5a8d2af36e193eeb6b4dfc6a07))
- deps: Update Rust crate testcontainers to v0.26.3 ([ac91495](https://github.com/pando85/kaniop/commit/ac9149534fc2495c0be8abe0cc95a6b5d97d9c0c))
- deps: Update ghcr.io/rash-sh/rash Docker tag to v2.17.3 ([1f02f77](https://github.com/pando85/kaniop/commit/1f02f77d97feca2f341549a2796a61921b6bc36c))
- deps: Update ghcr.io/rash-sh/rash Docker tag to v2.17.4 ([6535374](https://github.com/pando85/kaniop/commit/653537401a0f1a43d36df0b8f296c114fce2df57))

## [v0.1.9](https://github.com/pando85/kaniop/tree/v0.1.9) - 2025-11-30

### Fixed

- chart: Update metrics naming convention to use `_total` suffix for counters ([910d2b4](https://github.com/pando85/kaniop/commit/910d2b4fc072efcc51ce2ea2978ebdc69f17ad9e))
- operator: Add `_total` suffix to counter metrics ([8a45559](https://github.com/pando85/kaniop/commit/8a45559e7b91f02c65e552a8d7287e8572113544))

## [v0.1.8](https://github.com/pando85/kaniop/tree/v0.1.8) - 2025-11-30

### Fixed

- operator: Append '# EOF' marker to OpenMetrics output ([c8b67c6](https://github.com/pando85/kaniop/commit/c8b67c60dd1206071fa87e511a0e3ca788db0e7b))

## [v0.1.7](https://github.com/pando85/kaniop/tree/v0.1.7) - 2025-11-29

### Build

- chart: Add empty passphrase for helm oci chart release in Helm ([44014cf](https://github.com/pando85/kaniop/commit/44014cfd3107aa1f9e2bb775691eedff136b04df))

## [v0.1.6](https://github.com/pando85/kaniop/tree/v0.1.6) - 2025-11-29

### Build

- chart: Fix import GPG in Helm release ([bf74bac](https://github.com/pando85/kaniop/commit/bf74bac8e5f1f2141070e8802e1949e6789d8f13))

## [v0.1.5](https://github.com/pando85/kaniop/tree/v0.1.5) - 2025-11-29

### Added

- Implement OpenTelemetry-based Prometheus text exporter ([240d3a2](https://github.com/pando85/kaniop/commit/240d3a23f049c1ade6d3db29adfa268d08a600c5))

### Build

- chart: Fix create and use signing keyring ([36f9d83](https://github.com/pando85/kaniop/commit/36f9d83d5b550a55cdff40436e9c621ecb694064))
- Remove prometheus-client dependency ([88f53ff](https://github.com/pando85/kaniop/commit/88f53ffb32b7bc77aa92fe5eaf7354241f098787))

## [v0.1.4](https://github.com/pando85/kaniop/tree/v0.1.4) - 2025-11-29

### Build

- chart: Add GPG signing support for Helm chart releases ([b9a7f75](https://github.com/pando85/kaniop/commit/b9a7f759e608375698dd14f18ea07f65de215447))
- chart: Add icon to `Chart.yaml` ([9999829](https://github.com/pando85/kaniop/commit/9999829025119c1e30a0b7f15493ed4a4d0b9aa7))

## [v0.1.3](https://github.com/pando85/kaniop/tree/v0.1.3) - 2025-11-29

### Documentation

- chart: Remove duplicated text at README.md ([120dd2d](https://github.com/pando85/kaniop/commit/120dd2dc73bda3cde543a220e95d5d7069977006))

### Build

- deps: Update Kanidm to 1.8.3 ([2f372ac](https://github.com/pando85/kaniop/commit/2f372aca5547e8d9cd9ea1d05a4854ac3e704c71))
- ci: Group Kanidm packages together in renovate ([c3aee44](https://github.com/pando85/kaniop/commit/c3aee44c7b2e7d785e3fffcd4fa8c8834aaf9866))
- deps: Update Rust crate clap to v4.5.52 ([c88ad44](https://github.com/pando85/kaniop/commit/c88ad44fc6d0144fa7a3d6a029c241823d106edb))
- deps: Update Rust crate clap to v4.5.53 ([a03a9b9](https://github.com/pando85/kaniop/commit/a03a9b97909a92b3f31ae381de84c29ca7536bd6))
- deps: Update actions/checkout action to v6 ([470b683](https://github.com/pando85/kaniop/commit/470b6835cafedeef0edb3812b1bf4462dc1d96f2))
- deps: Update Rust crate http to v1.4.0 ([5cdf373](https://github.com/pando85/kaniop/commit/5cdf3733a348ebdc54449b7a64c4aaea666dbfbf))
- deps: Update registry.k8s.io/ingress-nginx/kube-webhook-certgen Docker tag to v1.6.5 ([e5c78f5](https://github.com/pando85/kaniop/commit/e5c78f516bfb1d11b6561fe57e43a2332cbcf9b9))
- deps: Update tokio-tracing monorepo ([5f31fa8](https://github.com/pando85/kaniop/commit/5f31fa8cf70a278b3482c1520a180621b7939e37))
- deps: Update Rust crate hostname to v0.4.2 ([eb566fb](https://github.com/pando85/kaniop/commit/eb566fbb50c4ef0a78f99ff07d72a841269e3f5b))
- Free space before publish packages ([514c690](https://github.com/pando85/kaniop/commit/514c690222c29508ee94e880a93533b62a13c1e4))
- chart: Disable plugin install unittest verification ([e511974](https://github.com/pando85/kaniop/commit/e511974d922f0bbb69d563f0e96466e883f2fa96))

## [v0.1.2](https://github.com/pando85/kaniop/tree/v0.1.2) - 2025-11-16

### Added

- ci: Add commit-msg check in pre-commit action ([7c4a641](https://github.com/pando85/kaniop/commit/7c4a64139333f11d6f7c13f612e1d94e51781c63))
- ci: Add rerun when e2e fails ([5505fdf](https://github.com/pando85/kaniop/commit/5505fdfb68eb261b292e214739ca54dfc3b0a2d2))

### Fixed

- chart: Correctly configure port on operator and webhook and allow configure envars in webhook ([0b4e403](https://github.com/pando85/kaniop/commit/0b4e403c3f6d3c988ae9a43e985479e9386d3cf7))
- Make listen address configurable for operator and webhook ([39a7d2c](https://github.com/pando85/kaniop/commit/39a7d2cb9e67e53ea6540bb09a5d0ca86673a8f1))

### Documentation

- chart: Add Kaniop update version with release notes in artifacthub changes ([92d735b](https://github.com/pando85/kaniop/commit/92d735bc6b23e93fadf90b488a251454abe50984))
- Add details to contributing.md ([14f7351](https://github.com/pando85/kaniop/commit/14f73518485d697149b238d54a1b3815c569cae3))
- Add CLAUDE.md file ([1a597a0](https://github.com/pando85/kaniop/commit/1a597a013e9cdb18fd64b692f57d87a65a0fa98b))

### Build

- deps: Update Rust crate axum to v0.8.7 ([c72ee8c](https://github.com/pando85/kaniop/commit/c72ee8c9526f6c51b5da8f2d2d727f857fa3d059))
- deps: Update Rust crate axum-server to v0.7.3 ([1ca6276](https://github.com/pando85/kaniop/commit/1ca6276863ef98384dfd3f6958cf0db7b9b08d61))
- deps: Update Rust crate hyper-util to v0.1.18 ([610d652](https://github.com/pando85/kaniop/commit/610d652a27a9b5aee6e3dfe7fb6b804f2d5d4567))

### Testing

- Retry workflows just once ([38113d1](https://github.com/pando85/kaniop/commit/38113d1dac779405637ea12b137d6b8f40506872))

## [v0.1.1](https://github.com/pando85/kaniop/tree/v0.1.1) - 2025-11-11

### Fixed

- kanidm: Add stdout to upgrade-check failure ([f3bc959](https://github.com/pando85/kaniop/commit/f3bc959254bcf93fe5447b738070555ca3446d85))

### Documentation

- chart: Add readme ([59908dd](https://github.com/pando85/kaniop/commit/59908dd41af8899583760a4f1d29c6acb502bea3))
- chart: Add changes to ArtifactHub changes annotation ([5a880ba](https://github.com/pando85/kaniop/commit/5a880ba0706244a498b9a2da8611dd78fd54709e))
- chart: Add artifacthub-repo.yaml to verify chart ([0300ec1](https://github.com/pando85/kaniop/commit/0300ec16d34ff5fe87286ddd798030c80c94e864))
- Include service accounts in the README ([95390dc](https://github.com/pando85/kaniop/commit/95390dcddabbb2e21121ea47eabac6ccc2676aeb))

## [v0.1.0](https://github.com/pando85/kaniop/tree/v0.1.0) - 2025-11-08

### First Official Release

Kaniop is now available as an official release! After extensive beta cycles, this marks our first supported version for real-world use.

### API Stability

This is a production-grade release, but the API and CRDs are not yet considered stable. We welcome feedback and may introduce breaking changes as we refine the design toward 1.0.

### Thank You

Thank you to all early adopters, testers, and contributors for your feedback and support!

### Fixed

- ci: Skip tests for rust matrix jobs ([15aecdb](https://github.com/pando85/kaniop/commit/15aecdb4e746ee5f57b28ca79db70ea7994ba152))
- cmd: Update ehalth check endpoints to /livez and /readyz ([65615f0](https://github.com/pando85/kaniop/commit/65615f0dae7f27be6c295c9b011db6b7fe050839))
- kanidm: Change verbosity level of upgrade check results ([4012830](https://github.com/pando85/kaniop/commit/4012830c7cddf079930943a7985b5fc5baa846bd))
- operator: Set config and TLS secret file permissions to 0400 ([b8789d2](https://github.com/pando85/kaniop/commit/b8789d24d1e8cc9fcf9227cc99e41473ad818b94))
- operator: Add replication service annotations and labels correctly ([b9a2afc](https://github.com/pando85/kaniop/commit/b9a2afc3e93a896d9ab1cf6b5864a022e4167dca))
- webhook: Increase TLS reload debounce to 5s ([a44f21c](https://github.com/pando85/kaniop/commit/a44f21c542c631f95528adcceb6bf04277281349))

### Documentation

- webhook: Add validating webhook documentation and index entry ([5cb4810](https://github.com/pando85/kaniop/commit/5cb481005e7017fae14f0aac393cd384981148b2))
- Change entryManagedBy in service account example to my-group ([db71e6d](https://github.com/pando85/kaniop/commit/db71e6deac4ccbe4a7175f6fda860677f0994529))
- Minor markdown fix ([67cdb62](https://github.com/pando85/kaniop/commit/67cdb623ac6d962de945696538e95f5ffc4ca03d))

### Build

- deps: Update Rust crate openssl to v0.10.75 ([5097925](https://github.com/pando85/kaniop/commit/5097925847e2196b71550151ab00e0abf4c2c96f))

### Styling

- Enforce sentence-case for commit subject ([375f5c4](https://github.com/pando85/kaniop/commit/375f5c470fd1cbc5f255530e2135b08687f60909))

## [v0.0.0-beta.11](https://github.com/pando85/kaniop/tree/v0.0.0-beta.11) - 2025-11-07

### Added

- Add admission webhook ([a91de76](https://github.com/pando85/kaniop/commit/a91de766040d554665079be632817d61d2d9d6e6))
  - **BREAKING**: The plural name for KanidmGroup CRD has changed from
"groups" to "kanidmgroups".

### Fixed

- chart: Rename PrometheusRules to kaniop and fix regex syntax ([47d9a41](https://github.com/pando85/kaniop/commit/47d9a41bdda94bce564e8d74deaecf29e8498785))
- oauth2: Add context to K8s event when OAuth2 group is missing ([e866fe7](https://github.com/pando85/kaniop/commit/e866fe7fd6472f94b76511d630e1443f4d5d75ca))

### Documentation

- chart: Add webhook documentation ([e992ef6](https://github.com/pando85/kaniop/commit/e992ef6db46793989e6f7aa20f38e0c090a35ae5))
- kanidm: Add trust proxy forward for example ([a6fd75c](https://github.com/pando85/kaniop/commit/a6fd75ce07cb7e2b11399560e0a2c8a538a48881))

### Build

- ci: Normalize image targets and force release for push-image recipes ([10a610b](https://github.com/pando85/kaniop/commit/10a610b4546eaae9e9d8c8c570d158def594de6a))
- deps: Update Rust crate tokio-util to v0.7.17 ([eb2c7c9](https://github.com/pando85/kaniop/commit/eb2c7c96d4706a4bb25ded9b72da4dd60e5a04a8))
- deps: Update Rust crate schemars to v1.0.5 ([7a4ae4c](https://github.com/pando85/kaniop/commit/7a4ae4c4fc21f60f51bc4c157198d663e88039a3))
- deps: Update ghcr.io/rash-sh/rash Docker tag to v2.17.2 ([b322b8f](https://github.com/pando85/kaniop/commit/b322b8fcfe1e396c3e0e35143a2c3e2a66f05c1d))
- deps: Update Rust crate rustls to v0.23.35 ([b739c8f](https://github.com/pando85/kaniop/commit/b739c8f7232014bad30e1fe1b6a58e37ef792cf3))
- deps: Update Rust crate schemars to v1.1.0 ([cbcb1c9](https://github.com/pando85/kaniop/commit/cbcb1c9426f0d543f0fbe4c210be076bcf919c69))
- deps: Update helm/kind-action action to v1.13.0 ([93e51a4](https://github.com/pando85/kaniop/commit/93e51a47cf4f06014e851473927bc1cc7502df89))

## [v0.0.0-beta.10](https://github.com/pando85/kaniop/tree/v0.0.0-beta.10) - 2025-11-02

### Added

- kanidm: Add region ingress support ([345f3e8](https://github.com/pando85/kaniop/commit/345f3e8fe821981bc1f8349287a88161708dbc10))
- kanidm: Add replica group services for multi-region deployments ([7c86d2b](https://github.com/pando85/kaniop/commit/7c86d2be03e4fe9206275c636f9f952bce3ff47a))
- sa: Add support for generating credentials ([68a0734](https://github.com/pando85/kaniop/commit/68a07347819e33864d3ee9934648c0335bd336e3))

### Fixed

- chart: Check 4|5xx status code for KaniopK8sApiErrors rule ([25f177e](https://github.com/pando85/kaniop/commit/25f177ecc9bef6d5150b3e9fcd2286fc0395c20f))
- kanidm: Correct ingress deletion logic ([ea688da](https://github.com/pando85/kaniop/commit/ea688da4b08bbf749247163db6c97789af4998a8))
- kanidm: Update cert replication host when need it ([f4e6947](https://github.com/pando85/kaniop/commit/f4e6947f56f5102e20699e631321ec54a38e9028))
- kanidm: Allow patch version upgrades when upgrade check fails ([7ceaf27](https://github.com/pando85/kaniop/commit/7ceaf276f8dbd12f6930898984ccf045c406f920))
- operator: Watch only metadata on secret stores for OAuth2 and SA ([3b688e1](https://github.com/pando85/kaniop/commit/3b688e16dfcec4e8f05ed405ab27dd2b69dd7461))
- sa: Handle edge case where token has default name ([e9b4eb0](https://github.com/pando85/kaniop/commit/e9b4eb05ff06b5b085f9fa3a301707946bde2184))
- Make clippy happy for rust 1.91.0 ([a197071](https://github.com/pando85/kaniop/commit/a197071289428618ea12996105ab626673fc9086))

### Documentation

- Extract enum default from schema property ([37287f2](https://github.com/pando85/kaniop/commit/37287f2a49503dfe53494aa6e465ecb552872a88))

### Build

- deps: Update Rust crate rustls to v0.23.34 ([afaa2eb](https://github.com/pando85/kaniop/commit/afaa2eb438ec8489fc094600281f4733086abf63))
- deps: Update Rust crate testcontainers to v0.25.2 ([7e72615](https://github.com/pando85/kaniop/commit/7e7261523b4148b5e14290ea50961dbdd26f297b))
- deps: Update Rust crate clap to v4.5.51 ([222bb43](https://github.com/pando85/kaniop/commit/222bb43f0b0ea8c402147f1a133b7909e0599f19))
- deps: Update pre-commit hook renovatebot/pre-commit-hooks to v41.168.1 ([7b5b7e0](https://github.com/pando85/kaniop/commit/7b5b7e0736a6d0fbde56434365cdf33448a14ed7))
- deps: Update pre-commit hook renovatebot/pre-commit-hooks to v41.168.3 ([1399568](https://github.com/pando85/kaniop/commit/13995688b4aad57295c179d6deb233d6014527b9))

### Testing

- ci: Reduce parallelism to 3 in ARM e2e tests ([1c55324](https://github.com/pando85/kaniop/commit/1c553243446b289ee3021edcdeda312dac4f2069))
- ci: Increase concurrency in e2e ([146ce42](https://github.com/pando85/kaniop/commit/146ce42cb91a599cd770287e8c0d78c98e263101))
- Increase e2e `wait_for` timeout to 180s ([4720569](https://github.com/pando85/kaniop/commit/47205698ec44a907b3b1aa9533b750bf3d7e58f2))

## [v0.0.0-beta.9](https://github.com/pando85/kaniop/tree/v0.0.0-beta.9) - 2025-10-22

### Added

- kanidm: Add upgrade pre-checks ([4c66970](https://github.com/pando85/kaniop/commit/4c669703a9ad49e5d5237044a7962070204928a6))

### Fixed

- kanidm: Ensure `tlsSecretName`s are a Kubernetes resource name by policy ([0c609f3](https://github.com/pando85/kaniop/commit/0c609f3816cc7f9d86dc9a4109000863e0e3d11e))
- operator: Add Mutex to prevent concurrent Kanidm client creation ([9416256](https://github.com/pando85/kaniop/commit/9416256b4b234aed8e203ace499d04c2ab6c4b52))

### Documentation

- kanidm: Update KEP-4650 link about PVC resizing ([807ad35](https://github.com/pando85/kaniop/commit/807ad35747ea8253a84495be2d4d7719455547be))
- Add commit ID links in CHANGELOG.md ([1a06fcf](https://github.com/pando85/kaniop/commit/1a06fcf92d815d4cd4f5b00b45c6e9f43aeeef6f))

### Build

- deps: Update Rust crate backon to v1.6.0 ([63d5171](https://github.com/pando85/kaniop/commit/63d517127c67b2135f231fbe9aac193e45022673))
- deps: Update Rust crate clap to v4.5.50 ([687c07d](https://github.com/pando85/kaniop/commit/687c07dd2385d51d1aa1c457d2912ca154de2b22))

### Refactor

- kanidm: Handle status error before reconcile function ([2ad63fd](https://github.com/pando85/kaniop/commit/2ad63fdb576eb4b4496f5d68f28c55cc3a0d1181))
- Change `to_owned` `to_string` for str ([a82a8e9](https://github.com/pando85/kaniop/commit/a82a8e9a935462b48d8fd827fd82689431d85ce0))
- Move regex schema validations to CRD when possible ([d9d320d](https://github.com/pando85/kaniop/commit/d9d320da8a6a37739fcffc2f5af34c502f421a31))

### Testing

- Improve wait_for error message ([848302c](https://github.com/pando85/kaniop/commit/848302c00d5414f95ca5c0662f1cf926feb78dda))
- Increase wait_for dutation to 90s ([a709de6](https://github.com/pando85/kaniop/commit/a709de69ea64925aa533ddc01e7479579bc098b5))
- Run pre-commit clippy in all features ([9f0c9cd](https://github.com/pando85/kaniop/commit/9f0c9cdeade20414fda91c1cbf8653d6c28e5064))

## [v0.0.0-beta.8](https://github.com/pando85/kaniop/tree/v0.0.0-beta.8) - 2025-10-17

### Added

- chart: Minor improvements in dashboard ([a2d99e1](https://github.com/pando85/kaniop/commit/a2d99e1d0b51f2ac3e6a8cb59fddd950961eba04))
- sa: Add service account controller ([d640873](https://github.com/pando85/kaniop/commit/d64087360e8cd126dd34c2e330692342a8fada31))

### Build

- deps: Update Rust crate openssl to v0.10.74 ([3c00762](https://github.com/pando85/kaniop/commit/3c0076220eb96a339a02cd97968d81f81038b0a3))
- deps: Update Rust crate rustls to v0.23.33 ([f453442](https://github.com/pando85/kaniop/commit/f4534426b15d335af2ba97a813689873232d4412))

## [v0.0.0-beta.7](https://github.com/pando85/kaniop/tree/v0.0.0-beta.7) - 2025-10-15

### Added

- Tune metrics and add dashboard and prometheusrules ([38ada34](https://github.com/pando85/kaniop/commit/38ada343c12e7ef8a15e252d090921895f0fb73c))

### Fixed

- Free unused memory and refine lock usage ([97a2ba2](https://github.com/pando85/kaniop/commit/97a2ba291417ebedec309b4c7a0637a31160b540))
  - **BREAKING**: After update you have to clean up old finalizers. Execute:
```bash
for resource in kanidmgroup person oauth2; do
  kubectl get $resource -A -o \
    custom-columns='NAMESPACE:.metadata.namespace,NAME:.metadata.name' \
    --no-headers 2>/dev/null | \
    while read ns name; do
      kubectl -n "$ns" patch $resource "$name" \
        -p '{"metadata":{"finalizers":[]}}' --type=merge || true
    done
done
```

### Documentation

- Remove implemented TODO comment ([5377ef1](https://github.com/pando85/kaniop/commit/5377ef1da53bc5b3bd14f0cb5f66576d34e01e5f))

### Build

- deps: Update Rust crate clap to v4.5.49 ([3e3abb9](https://github.com/pando85/kaniop/commit/3e3abb94cf522fbff1230f861033f3bf9f19c83a))
- deps: Update Rust crate tokio to v1.48.0 ([a91bf0b](https://github.com/pando85/kaniop/commit/a91bf0bf99afc76b85679ce49c02c376921af15c))
- deps: Update ghcr.io/rash-sh/rash Docker tag to v2.16.2 ([55e068e](https://github.com/pando85/kaniop/commit/55e068e08909c2f4999feda81e035869d96bed4e))
- deps: Update Kanidm to 1.7.4 and add rustls dependency ([590e6a8](https://github.com/pando85/kaniop/commit/590e6a8c8a137d96cacd579bd8484e27c83b2199))

## [v0.0.0-beta.6](https://github.com/pando85/kaniop/tree/v0.0.0-beta.6) - 2025-10-12

### Added

- ci: Skip tests in documentation only changes ([afaa94e](https://github.com/pando85/kaniop/commit/afaa94ebdfd1623ca1b964fc1e9a17c28bd0740c))
- kanidm: Allow origin configuration ([e9ba78e](https://github.com/pando85/kaniop/commit/e9ba78e566b0706c06cd6784e519ecb521d4a921))
- kanidm: Renew replication certificates ([685f478](https://github.com/pando85/kaniop/commit/685f4786c589cf959ac9d1b3ef481706b18b742c))
  - **BREAKING**: Replica certificates secrets has to be recreated after
this change for being able to renew them.

### Documentation

- book: Update to use versioned example links ([f57f432](https://github.com/pando85/kaniop/commit/f57f43230151da6db3df0c304c9bd6b51de2c5d2))
- kanidm: Clarify ingress requirements about TLS and session affinity ([3be2a0d](https://github.com/pando85/kaniop/commit/3be2a0dda6369e853744e83e4efb95aad5a06f03))
- kanidm: Add usage guide and update docs ([0c7cac3](https://github.com/pando85/kaniop/commit/0c7cac3701dc18b602f1625bd4f3cb6b5c8c4b91))

## [v0.0.0-beta.5](https://github.com/pando85/kaniop/tree/v0.0.0-beta.5) - 2025-10-11

### Fixed

- kanidm: Not include ingress `extraTlsHosts` in non TLS configuration ([ed6d8c7](https://github.com/pando85/kaniop/commit/ed6d8c79f74f8457769b3bfa58b3bc5c4c71e7f8))

### Documentation

- kanidm: Improve namespace selector docs ([e6b19cc](https://github.com/pando85/kaniop/commit/e6b19cc27665fb7b8f998080016338f6bbe6b6d9))

## [v0.0.0-beta.4](https://github.com/pando85/kaniop/tree/v0.0.0-beta.4) - 2025-10-11

### Added

- kanidm: Add extra TLS hosts config for ingress secret ([a430af5](https://github.com/pando85/kaniop/commit/a430af5e8aa9e453f2078eaf8feeb43b816e382e))

### Fixed

- ci: Run public crates once ([64ddd5e](https://github.com/pando85/kaniop/commit/64ddd5ee469d6c6ca0aa0298adf123b0da9d3df9))

### Documentation

- Fix container image badge ([57e7480](https://github.com/pando85/kaniop/commit/57e748006e9d6ba65cb4b53795f607a7c775c645))

## [v0.0.0-beta.3](https://github.com/pando85/kaniop/tree/v0.0.0-beta.3) - 2025-10-09

### Added

- chart: Add admission policies for BTreeSet uniqueness validation ([0cb088d](https://github.com/pando85/kaniop/commit/0cb088d0aaf5412c739ccb573f6b5a2f0a187167))
- chart: Add admission policy rules for scopes and claims uniqueness validation ([3695ac4](https://github.com/pando85/kaniop/commit/3695ac48fcccba3e133eb68e5597bcfd828e3fd3))
- ci: Run aarch64 target test on arm runners ([f207f44](https://github.com/pando85/kaniop/commit/f207f44d64bc0bd17025f0c78cb8b3ff61431cb2))
- crd: Add ages column to person, group and Oauth2 CRDs ([27931aa](https://github.com/pando85/kaniop/commit/27931aaf52417fa1dd651aeb307fe6854df9a805))

### Fixed

- oauth2: Sort scopes and values in normalize for correct comparison ([762faa1](https://github.com/pando85/kaniop/commit/762faa125ef866a997eaf2261c8f6852ab2eb6e4))
- Change `Vec` to `BTreeSet` on mail and members ([f4e4ba6](https://github.com/pando85/kaniop/commit/f4e4ba65e19d8f5a3666b559d2fbafece64f998a))

### Documentation

- Add comments on ordered comparisons ([40e6ef9](https://github.com/pando85/kaniop/commit/40e6ef9f627fc8a0733130e0e1e9a4ff4f63e23a))

## [v0.0.0-beta.2](https://github.com/pando85/kaniop/tree/v0.0.0-beta.2) - 2025-10-09

### Added

- group: Support cross namespace reference ([714692f](https://github.com/pando85/kaniop/commit/714692f55a657b82da453cb65d95b18e7f773ce6))
- person: Support cross namespace reference ([fe2220b](https://github.com/pando85/kaniop/commit/fe2220b9e330435f8abb345f7f1de9b4686fba38))

### Documentation

- kanidm: Add default security context ([8bb6a30](https://github.com/pando85/kaniop/commit/8bb6a30de31052fd149b5e2f03864574fa1af85d))
- oauth2: Add namespace to kanidm ref and secret creation documentation ([b0748dc](https://github.com/pando85/kaniop/commit/b0748dcb0c0e46c36a1b2ceb898f095a4d4eaebc))

### Build

- ci: Fix cargo login token ([4c1dad2](https://github.com/pando85/kaniop/commit/4c1dad2f99c56293d67f94615ffcef1810dfac6f))
- deps: Update Rust crate serde to v1.0.228 ([fc9bf73](https://github.com/pando85/kaniop/commit/fc9bf73b67c75424728819d0ca6bafd077495b96))
- deps: Update Rust crate axum to v0.8.6 ([c28f83f](https://github.com/pando85/kaniop/commit/c28f83f4b5bfc9dae1b8f2279d1ae9776c0a68f0))
- deps: Update Rust crate thiserror to v2.0.17 ([658d452](https://github.com/pando85/kaniop/commit/658d452f3f50c4bdc9997c242e8d8723995cd46e))
- Fix cargo publish and change to `--workspace` ([8d04a31](https://github.com/pando85/kaniop/commit/8d04a310042a2ec94b774f957f4c7f6d3a4ef319))

### Refactor

- operator: Implement KanidmResource trait in library ([ef20161](https://github.com/pando85/kaniop/commit/ef20161a97bad54d2d89f87a560589506cd0f278))
- operator: Move `is_resource_watched` logic to library ([ff49a65](https://github.com/pando85/kaniop/commit/ff49a65934755604d56ff10327c4f2b8e6e14495))

### Testing

- group: Add group namespace selector e2e test ([e2207de](https://github.com/pando85/kaniop/commit/e2207de88a43323e4a3eee441b9c36121054c4e7))

## [v0.0.0-beta.1](https://github.com/pando85/kaniop/tree/v0.0.0-beta.1) - 2025-10-05

### Added

- operator: Make KanidmRef as inmutable ([4cb0044](https://github.com/pando85/kaniop/commit/4cb0044f5315ad15a6d6eff7199e2080dcc04e83))
- person: Make reset cred token TTL configurable ([bfb6980](https://github.com/pando85/kaniop/commit/bfb69807e1dee2a4613b23bbb0d1b633a8788d5a))

### Fixed

- ci: Remove dead create manifest code on docker images workflow ([ce1f058](https://github.com/pando85/kaniop/commit/ce1f05801a30db4f564a97c3964dc4e853ea2999))
- ci: Add fmt and clippy for build tests ([8582d35](https://github.com/pando85/kaniop/commit/8582d3522eabee7a5aea44f5beebe733a6c34632))
- kanidm: Make PersistentVolumeClaim metadata field optional ([336f57d](https://github.com/pando85/kaniop/commit/336f57de7d6fbd22aa095ed1c4a55b98d5499260))

### Documentation

- kanidm: Add LDAP port protocol docs ([fe3c68c](https://github.com/pando85/kaniop/commit/fe3c68ce7406be2a9a3efecf1742fe41d22601ea))
- Add copilot instructions ([ed3d9dd](https://github.com/pando85/kaniop/commit/ed3d9dd8323baced463c3e12966d78c562017451))
- Add examples-gen feature ([1eba221](https://github.com/pando85/kaniop/commit/1eba2216e8372331279db2fc8ed7133b085660ab))
- Add enum options with default markers to examples ([90aa8e8](https://github.com/pando85/kaniop/commit/90aa8e8274ee3ccee8c0e48d411209726fa113d0))
- Fix quickstart guide for getting Kanidm working ([449a379](https://github.com/pando85/kaniop/commit/449a3795a7b7eaedd874dc4e3392faf8caaacb2a))

### Build

- ci: Add `tracing-opentelemetry` to opentelemetry renovate PRs ([dde13ea](https://github.com/pando85/kaniop/commit/dde13ea512c9ea865f19972a39190e52074a36a0))
- deps: Update actions/checkout action to v5 ([6c4f95d](https://github.com/pando85/kaniop/commit/6c4f95d0ef138e1a926ded4d4ef11dac47c98f13))
- deps: Update opentelemetry ([a69dadc](https://github.com/pando85/kaniop/commit/a69dadc3c98641edcea273cee66dbfb816b19616))
- deps: Update Cargo.lock ([90d45c8](https://github.com/pando85/kaniop/commit/90d45c8f28d553cdb5dd2820fc53c6c4dc9fcf23))
- deps: Update pre-commit hook alessandrojcm/commitlint-pre-commit-hook to v9.23.0 ([87f0c2f](https://github.com/pando85/kaniop/commit/87f0c2f0a2dde0e417111f828b715157e9ebc59a))

### Testing

- ci: Add verify examples tests ([42d74b0](https://github.com/pando85/kaniop/commit/42d74b0318f7d1e18a00f673cda5f9fed4faade1))

## [v0.0.0-beta.0](https://github.com/pando85/kaniop/tree/v0.0.0-beta.0) - 2025-09-23

### Added

- chart: Add livenessProbe ([c4d6c05](https://github.com/pando85/kaniop/commit/c4d6c05276b5b721230fd977ac22f4ba66068189))
- chart: Add validating admission policy for checking names ([475186b](https://github.com/pando85/kaniop/commit/475186be4536773aa40f2b10cfa940505c08e903))
- ci: Add scope-enum to commitlintrc ([b1d2600](https://github.com/pando85/kaniop/commit/b1d2600cc5326615e6075917d51953fdce7aae7f))
- ci: Enable pre-commit renovate updates ([c263256](https://github.com/pando85/kaniop/commit/c263256d1b0d667670e6d2163a84310ac39197ff))
- ci: Add renovate auto migrate configuration ([adf6235](https://github.com/pando85/kaniop/commit/adf62352219c541902490722f35a0c33df059784))
- error: Add context and deprecate metrics error labels ([7bceac1](https://github.com/pando85/kaniop/commit/7bceac116e6576bb453ce5451b90fe5116b50766))
- group: Add controller ([2871582](https://github.com/pando85/kaniop/commit/287158260da69cf7b96f64c74548e9a960279a6f))
- group: Enchance CRD columns with new status fields ([d628a70](https://github.com/pando85/kaniop/commit/d628a7005ec381f2a0216e4b8760bd352ccf56b6))
- group: Enable `entryManagedBy` field ([aedc590](https://github.com/pando85/kaniop/commit/aedc5907be495e88b92580bbb18600809ecd8d3b))
- k8s-util: Add recorder with aggregation logic ([e973e84](https://github.com/pando85/kaniop/commit/e973e84714846af616aa5f54616fd83a7cd4baf1))
- kanidm: Add ingress, service and LDAP configuration ([626b718](https://github.com/pando85/kaniop/commit/626b718696d71aeecfdf3d021d732e7b438747ad))
- kanidm: Add storage generation ([9494d02](https://github.com/pando85/kaniop/commit/9494d029f3cf447ca0edda7f5d549ca6aaeee7f4))
- kanidm: Add TLS configuration ([ef028e8](https://github.com/pando85/kaniop/commit/ef028e8f01fece90ab54a53eb7e69edb9d9c4d9a))
- kanidm: Add env to allow config params ([d9f0c6d](https://github.com/pando85/kaniop/commit/d9f0c6d09f5fe7c3abdfc7073b87cf111369d93d))
- kanidm: Allow service type and annotations configuration ([2456d1b](https://github.com/pando85/kaniop/commit/2456d1be922b36bd45f691bcd9c5e508b6bc0664))
- kanidm: Add services and ingress controller watchers and stores ([b763725](https://github.com/pando85/kaniop/commit/b76372515bfdbc94e115cc9eb0fe3e2a5e4254e2))
- kanidm: Use real statefulset, service and ingress ([e66c19e](https://github.com/pando85/kaniop/commit/e66c19e0b5ba32a605ebd1439cefc63964403d5b))
- kanidm: Rework Kanidm status ([22f8881](https://github.com/pando85/kaniop/commit/22f88814014912d8cdf26bc1832019349191611d))
- kanidm: Add secret watcher and store ([67a0bcb](https://github.com/pando85/kaniop/commit/67a0bcbb6a027fce4f551387327337c6caf2dddc))
- kanidm: Generate admin secrets ([9d49c1f](https://github.com/pando85/kaniop/commit/9d49c1f073526aca6d0b8a5c315c8f7481b5e56b))
- kanidm: Add initialized condition and e2e tests for admin secrets ([2bdc954](https://github.com/pando85/kaniop/commit/2bdc954d75aa80820bcde79a740ba7054701b813))
- kanidm: Add different replication groups support ([f2460e9](https://github.com/pando85/kaniop/commit/f2460e9b4f673f93c0e4b659827b87427ef19355))
- kanidm: Add external replication nodes configuration ([6181a45](https://github.com/pando85/kaniop/commit/6181a4550cfc700cfb7713be2a085efea100c9da))
- kanidm: Enhance CRD columns with new status fields ([a7711d9](https://github.com/pando85/kaniop/commit/a7711d94aff76cea51b9011a0db38673bcac950c))
- oauth2: Add controller ([5425e3c](https://github.com/pando85/kaniop/commit/5425e3c9d6e544ea97f6222450c2b7489304c68f))
- oauth2: Allow cross-namespace deployments ([6a53f40](https://github.com/pando85/kaniop/commit/6a53f4092f7ec792d09af9bcd12e2d007222f9d8))
- oauth2: Add secret ([b3b5754](https://github.com/pando85/kaniop/commit/b3b57546c4ca6e5610c48f765d26343318b2564e))
- oauth2: Final implementation of oauth2 secret with tests ([354f134](https://github.com/pando85/kaniop/commit/354f13481a718b9c81895fe78e7dc6d082efe868))
- oauth2: Enchance CRD columns with new status fields ([4bec7c4](https://github.com/pando85/kaniop/commit/4bec7c4bb216af2741e881ad21f641c6308ef71a))
- operator: Support multiple stores per context ([4243942](https://github.com/pando85/kaniop/commit/42439420fc5c37662ea621e3772aef7a4e1fdf53))
- operator: Add backoff when reconcile fails ([01965b7](https://github.com/pando85/kaniop/commit/01965b7523c5f929a3eccf0048cbee8c51ac85a6))
- operator: Add Kanidm system clients ([279482f](https://github.com/pando85/kaniop/commit/279482f105d453e71decfb0eb212b24124693b87))
- person: Add controller ([34db291](https://github.com/pando85/kaniop/commit/34db291b72937d1c1ee05bd6d7b5273f959444a7))
- person: Enable controller, finish feature and add tests ([82047c9](https://github.com/pando85/kaniop/commit/82047c997ff681201a2d78e84d59c2300ebbadb5))
- person: Add posix attributes ([0e75245](https://github.com/pando85/kaniop/commit/0e752458ca1fbeff31f03671ae221ba49bf64f13))
- person: Add credentials reset link ([b8ce7bf](https://github.com/pando85/kaniop/commit/b8ce7bf3db2eb8c91b4f2bef157cde05e8f712ee))
- person: Add event when update fails ([2b38ef1](https://github.com/pando85/kaniop/commit/2b38ef1b1c2bfc75ef3c0db4b66158e726a8f2d2))
- person: Enchance CRD columns with new status fields ([71cbd9d](https://github.com/pando85/kaniop/commit/71cbd9d543f5436b3663a3f689263afe426369a6))
- Add helm chart ([dc06b23](https://github.com/pando85/kaniop/commit/dc06b23bf8092c52d7be2560a20ed4c9f819c17f))
- Add clap for handling args and rework telemetry init ([50383bf](https://github.com/pando85/kaniop/commit/50383bf44078fa420556a6dc0abcfd641ca16296))
- Add owner references and react to changes on owned resources ([d034a07](https://github.com/pando85/kaniop/commit/d034a0774a0703a16a8067e8de3b4fc65912ee7a))
- Add state reconciliation ([ce5e5d6](https://github.com/pando85/kaniop/commit/ce5e5d649bc16c97f9761f35302075c625374904))
- Add status.conditions and ready column ([cde8e4a](https://github.com/pando85/kaniop/commit/cde8e4a34e889f6edd022239d28e4fb84d682876))
- Add echo status tests and refactor reconcile ([5f37d05](https://github.com/pando85/kaniop/commit/5f37d05c8e1b6797ee2b780553d2b4d6fbf0f16b))
- Add e2e tests ([0eac052](https://github.com/pando85/kaniop/commit/0eac05234fe9e4cbb2940bef3fc4901773f11e60))
- Add kubernetes client metrics ([1809120](https://github.com/pando85/kaniop/commit/18091205097fc4e7bd894206e2bf146af266fd22))
- Add metrics to kubernetes client requests total per status code ([3881e61](https://github.com/pando85/kaniop/commit/3881e614811d1ae94d38a7c2649353e2dd8b21ff))
- Add metrics to controller ([9228cb0](https://github.com/pando85/kaniop/commit/9228cb04e03f0e7b2dec34365ee0774b06e7e0f2))
- Change to crdgen and implement StatefulSetExt for Kanidm ([aff6378](https://github.com/pando85/kaniop/commit/aff6378f15b9c0ec7717f4ec2820c7f46b82740c))
- Add transparent and svg logo ([0a1a1b2](https://github.com/pando85/kaniop/commit/0a1a1b245125f316827672e6ad1926a30e5943ec))
- Add Kanidm store to Context ([3867961](https://github.com/pando85/kaniop/commit/3867961319aa497de8b38196142be74bd382ec7c))
- Split `controller::Context` and create `kanidm::Context` ([a1f4da4](https://github.com/pando85/kaniop/commit/a1f4da42a1adb9fa46675110a73aa6e33d0aaff6))
- Split person `Context` ([331ac5b](https://github.com/pando85/kaniop/commit/331ac5bb62ec3c52b42fcd786527ba5b95692142))
- Add kanidm_ref to columns ([52a9541](https://github.com/pando85/kaniop/commit/52a9541f01c909946ae7d29e6a15ededebf61ce0))

### Fixed

- chart: Truncate version label to 63 chars ([e6e7709](https://github.com/pando85/kaniop/commit/e6e77091ca77513bcc6329b95d632afb8ff049a9))
- chart: Version label equal to left side of `@` symbol ([011f137](https://github.com/pando85/kaniop/commit/011f137f8cb75018bc067bf0f3a9784e3a06c78f))
- ci: Remove deprecated `crd-code` target and add mkdir for crdgen ([95bf3eb](https://github.com/pando85/kaniop/commit/95bf3eb3e59fb90b539ef7fac42d2930b8dd99f4))
- ci: Clippy Github Action name typo ([f84a508](https://github.com/pando85/kaniop/commit/f84a508daa10efa41c634776864b23f63a33d4cd))
- ci: Just run e2e-tests in x86 and add cache for release target ([8359882](https://github.com/pando85/kaniop/commit/835988211990a5b90732cb31a16c4d7635102672))
- ci: Change log level to info in e2e tests ([94df7c8](https://github.com/pando85/kaniop/commit/94df7c8704941c99de6afa8db23b54f4c8a742c2))
- ci: Renovate update just patch versions of kind image ([f87acda](https://github.com/pando85/kaniop/commit/f87acdad06a02a7e3e221faa5b47342dad63dd6d))
- ci: Schedule renovate for `renovatebot/pre-commit-hooks` once per month ([868237d](https://github.com/pando85/kaniop/commit/868237dd0ae5b3bfa8f7eb0e0dafabcdfec3e61d))
- ci: Migrate pre-commit configuration ([0775bbc](https://github.com/pando85/kaniop/commit/0775bbcc9b70937c6bc67df3934a6dbd9c65b5ed))
- ci: Add permissions for publishing releases on github actions ([5be5ff3](https://github.com/pando85/kaniop/commit/5be5ff32f155ec6bc67251016ebea282185ea06e))
- ci: Add `.yml` files to enable kind ([d7cab7a](https://github.com/pando85/kaniop/commit/d7cab7a8a86350361cd6ea2129fe81d7c298b1ca))
- ci: Group opentelemetry update PRs ([2f97eca](https://github.com/pando85/kaniop/commit/2f97eca14e45c1ca189347e250cb1e353b03ee0a))
- ci: Configure mdbook version ([2f468a4](https://github.com/pando85/kaniop/commit/2f468a4ea04bfccbc167c3b272f3c1372940274a))
- ci: Remove `/opt/hostedtoolcache` dir on github actions runners ([a81f45e](https://github.com/pando85/kaniop/commit/a81f45e1fdb1075eb374329d60caefe0f99c0ad6))
- ci: Use buildx on push image ([bac4416](https://github.com/pando85/kaniop/commit/bac44165f861b544efa73c8f118e3722ce971fd5))
- ci: Use Cargo.toml version for updating changelog ([67371ae](https://github.com/pando85/kaniop/commit/67371ae4d3297def5ab0c844785c342c23b5a358))
- cmd: Handle SIGTERM signal ([b47fdca](https://github.com/pando85/kaniop/commit/b47fdca1f907cc121e3a4372759404eeb5716cce))
- crd: Add pattern for domain field and fix tests ([9bad417](https://github.com/pando85/kaniop/commit/9bad417d03c4260d7a0e16134bd8efd65f7b472c))
- crd: Add `kaniop` category ([97cab7d](https://github.com/pando85/kaniop/commit/97cab7d3d7e492d9ba3d2ff9930d9ad0c4b20f8b))
- k8s-util: Update events version from e8e4b54 ([6336cef](https://github.com/pando85/kaniop/commit/6336cefa09e5cd7055c52d4e04df4f2b2d2d9201))
- k8s-util: Update events version from d85f31 ([32234a6](https://github.com/pando85/kaniop/commit/32234a6086346ed282931424a6a8881e655d7a99))
- kanidm: Add service per pod for workaround replication ([460da6d](https://github.com/pando85/kaniop/commit/460da6d1cf190989875c04d5fc57cd4de37431aa))
- kanidm: Delete all objects at once using `store.state()` ([f438138](https://github.com/pando85/kaniop/commit/f438138f816ec23921c03e663659c9fb5c8a4177))
- kanidm: Add different keys for certs on replication configuration ([b8b7d72](https://github.com/pando85/kaniop/commit/b8b7d7219a5111c1d3b81116e1ef62b51c2645d2))
- kanidm: Change version to v1beta1 ([c167d64](https://github.com/pando85/kaniop/commit/c167d64841aec3f8c62f317de5d5126792867d18))
- Cleaner log messages reusing spans from kube_runtime and remove trace_id ([82c31cf](https://github.com/pando85/kaniop/commit/82c31cfcb8962310b3d4f3ea8ae86b817807f4af))
- Add trace_id to logs ([04f6786](https://github.com/pando85/kaniop/commit/04f6786019ad5e5f8b214fed17d8e4c8b794ddf5))
- Handle unwrap on metrics encode ([c9fecc5](https://github.com/pando85/kaniop/commit/c9fecc5f975b92e334d3074b920f6d7523cdc7ca))
- Handle unwraps in echo controller ([347a1a1](https://github.com/pando85/kaniop/commit/347a1a1dcd70170f1aa7a1ff42c7ea6511b36b9e))
- Clean small TODOs ([5fd1934](https://github.com/pando85/kaniop/commit/5fd1934b762954fe4818604279ff5dca91207c88))
- Add `metrics.status_update_errors_inc()` ([6b4bc30](https://github.com/pando85/kaniop/commit/6b4bc305c59c3e7f26a3e966be0bf936beae20c3))
- Correct crd status types and typo ([25de324](https://github.com/pando85/kaniop/commit/25de32431eb58994188960ad0bbe3aa0d874fc19))
- Replace `map_or` with `is_some_and` ([ffa8341](https://github.com/pando85/kaniop/commit/ffa83416f289d1261e8336399dfcd36bf7830549))
- Make clippy happy for rust 1.86.0 ([758f9ec](https://github.com/pando85/kaniop/commit/758f9ec7b040132d3ccf317bc0485c0b90a37af8))
- Use an empty dir volume for server config file ([5fd35f6](https://github.com/pando85/kaniop/commit/5fd35f6085588656522ee1370469117c083e1955))
- Make clippy happy for rust 1.87.0 ([9cad676](https://github.com/pando85/kaniop/commit/9cad6761a330bad19a65db4e02709db36bc471b2))
- Cargo clippy errors 1.88 ([60bfa31](https://github.com/pando85/kaniop/commit/60bfa31987336fddebd03d91937100e0e84b7e04))

### Documentation

- chart: Add artifacthub annotations ([0b3a4b9](https://github.com/pando85/kaniop/commit/0b3a4b979840d600e4de63c91937bc9c738dca81))
- kanidm: Add env link to Kanidm official documentation ([65f6c19](https://github.com/pando85/kaniop/commit/65f6c19c823ad590d69d20b99e6e5c77abce0873))
- person: Add posix person to examples ([41835d8](https://github.com/pando85/kaniop/commit/41835d8b7dad9152d5118cff0b66c832553af6d3))
- Add README features ([8cd17f6](https://github.com/pando85/kaniop/commit/8cd17f6b60ff84d74105c812a2d633c788008161))
- Show correct binary path in make build target ([cef8555](https://github.com/pando85/kaniop/commit/cef8555aeea87ebe150d7233805ab45aa183f494))
- Add logo ([dc82c71](https://github.com/pando85/kaniop/commit/dc82c710244aab02258279669898f42858e5a106))
- Fix logo URL ([6a8674d](https://github.com/pando85/kaniop/commit/6a8674d79572c4db7abdaef9dbaaf1332514c0c8))
- Generate examples programmatically ([ab1e7ef](https://github.com/pando85/kaniop/commit/ab1e7ef8ecb975a92bd26756d5c927863eea343e))
- Add supported versions ([197da06](https://github.com/pando85/kaniop/commit/197da0609eeaead03d10d680a47afbe8b576c92f))
- Update TODO comment PR number ([1dbf2de](https://github.com/pando85/kaniop/commit/1dbf2decb6a136e0e757c51c215206efb65f1481))
- Add book ([9bf0492](https://github.com/pando85/kaniop/commit/9bf04926f225f32af4c29b348f46313882983887))
- Fix URL links ([87a69e0](https://github.com/pando85/kaniop/commit/87a69e0fb157a41618c838d054755c9362f45b71))
- Reorder Oauth2 client, group and person quickstart ([9008f6d](https://github.com/pando85/kaniop/commit/9008f6dd13e2fd16c44df33620a713c308a066a5))
- Fix logic for commenting line if parent is optional ([33d4add](https://github.com/pando85/kaniop/commit/33d4add27359bed432e5efb4bbea88a5a8b459b1))
- Fix readme links ([5a5a302](https://github.com/pando85/kaniop/commit/5a5a3024af9475fd68f028e9a648c96397f9edca))

### Build

- ci: Change rash image to ghcr.io registry and add renovate ([0601a68](https://github.com/pando85/kaniop/commit/0601a68dd13c01249525cdc26a53967d759e3bd6))
- ci: Publish helm chart ([748b2ce](https://github.com/pando85/kaniop/commit/748b2ce7e5b4fe65b667d2a803f6c3c31c3e5ac0))
- ci: Add kind version to renovate ([a61d7f5](https://github.com/pando85/kaniop/commit/a61d7f524e2d966f2340bdd39412d7086860830d))
- ci: Fix repository name on Github Actions references ([5285f95](https://github.com/pando85/kaniop/commit/5285f953b45e53b8f000f345a65aad5d9b8a6c75))
- ci: Enable push-images on Makefile ([649fd3a](https://github.com/pando85/kaniop/commit/649fd3af0da98d78a36f2a341a69307ec1aa23b4))
- ci: Add `--provenance false` to docker buildx ([3f0a070](https://github.com/pando85/kaniop/commit/3f0a07037dadf4289c60b65b8187aade950510f3))
- ci: Fix docker multiarch image push ([9fd47c9](https://github.com/pando85/kaniop/commit/9fd47c950c1c5771e50896b94b0bdbbbd91ce625))
- ci: Fix CRD gen on helm release ([218c20d](https://github.com/pando85/kaniop/commit/218c20d4cfc0f101758bbcc4f29350afacb7b945))
- ci: Auto update renovate pre-commit once a month automatically ([b658b7c](https://github.com/pando85/kaniop/commit/b658b7ceb509513110a311e28027b34b6fb1674d))
- ci: Configure debian image to versioning loose in renovate ([58b335a](https://github.com/pando85/kaniop/commit/58b335ab2f575e474c3f2b5bf258cc7722699adf))
- ci: Fix `managerFilePatterns` expresions in renovate ([aa3052e](https://github.com/pando85/kaniop/commit/aa3052e6e974e961ae4452bd22b26f5186076913))
- ci: Handle mdbook version with renovate ([b1e11c5](https://github.com/pando85/kaniop/commit/b1e11c5e68b83bc5547f6ce18353bd7dbaac2b25))
- deps: Update Rust crate tonic to v0.12 ([3de1192](https://github.com/pando85/kaniop/commit/3de1192aa2cfccd398ad211ae52a04ea4baf690d))
- deps: Update Rust opentelemetry crates to v0.26 ([a624455](https://github.com/pando85/kaniop/commit/a6244557159a308918f8dc61db33a3978e6f2ec8))
- deps: Update Rust crate futures to v0.3.31 ([ac91e7e](https://github.com/pando85/kaniop/commit/ac91e7e17054228585ef24367f562eda9a9826d1))
- deps: Update Rust crate tokio to v1.40.0 ([6355861](https://github.com/pando85/kaniop/commit/635586152e7853e27ced60aeb8192c826b8a2c16))
- deps: Update Rust crate hyper to v1.5.0 ([66c21b8](https://github.com/pando85/kaniop/commit/66c21b8f21cf36a6f433a27c391f445e8202d51f))
- deps: Update kube-rs to 0.96 and tower to 0.5 ([89e78a4](https://github.com/pando85/kaniop/commit/89e78a475035bef1d78edc1daf928900009b10ea))
- deps: Update Rust crate serde_json to v1.0.129 ([92e5f6f](https://github.com/pando85/kaniop/commit/92e5f6f3aa6b714bece04bb7cd0302aae8d79542))
- deps: Update Rust crate anyhow to v1.0.90 ([3f836d9](https://github.com/pando85/kaniop/commit/3f836d958df7bd66677d915f2560463fff5ec169))
- deps: Update Rust crate serde_json to v1.0.130 ([fd19c16](https://github.com/pando85/kaniop/commit/fd19c165cc09f6e59e70ec012299d4c57bf2d064))
- deps: Update Rust crate serde_json to v1.0.131 ([e69538e](https://github.com/pando85/kaniop/commit/e69538e73c9ddffc9a13c8898b37c57ce9b17cf7))
- deps: Update Rust crate serde_json to v1.0.132 ([42da4bb](https://github.com/pando85/kaniop/commit/42da4bb575cc44deeafa8d310ab451e169e9f65a))
- deps: Update Rust crate serde to v1.0.211 ([4004dc7](https://github.com/pando85/kaniop/commit/4004dc7017dcad2c98da76878d36e7d226a2d880))
- deps: Update Rust crate serde to v1.0.212 ([511742a](https://github.com/pando85/kaniop/commit/511742a634b4acbe1633e5598609145844033a9f))
- deps: Update Rust crate thiserror to v1.0.65 ([7540519](https://github.com/pando85/kaniop/commit/7540519290104217072b2e4e645f5b903a86e053))
- deps: Update Rust crate anyhow to v1.0.91 ([012eeae](https://github.com/pando85/kaniop/commit/012eeae4924040789cefa8cf8095e36fe9f32508))
- deps: Update Rust crate serde to v1.0.213 ([074551e](https://github.com/pando85/kaniop/commit/074551e17827af5e222f036d3084c630862630f9))
- deps: Update Rust crate tokio to v1.41.0 ([bf0608a](https://github.com/pando85/kaniop/commit/bf0608af149b26e6647cc68bb691257eeafc0997))
- deps: Update Rust crate serde to v1.0.214 ([f3b3b49](https://github.com/pando85/kaniop/commit/f3b3b49f1607270cdf62572b16f5aeb52f602c4d))
- deps: Update Rust crate hyper-util to v0.1.10 ([a5e640d](https://github.com/pando85/kaniop/commit/a5e640d4f39ad9f0f1d33d4ed09cc7072e099688))
- deps: Update Rust crate tokio to v1.41.1 ([5e33fb2](https://github.com/pando85/kaniop/commit/5e33fb2f7b603ec72ce1607c7d5a19c07e3c394d))
- deps: Update ghcr.io/rash-sh/rash Docker tag to v2.9.0 ([5e7c315](https://github.com/pando85/kaniop/commit/5e7c3157f546330138d081ce24b937ba1584eba3))
- deps: Update Rust crate serde to v1.0.215 ([2855be6](https://github.com/pando85/kaniop/commit/2855be64cf47a959c3b7fa3ade90e5fa7320925e))
- deps: Update Rust crate clap to v4.5.21 ([71b2580](https://github.com/pando85/kaniop/commit/71b258054ce2315a98c4f442b8339c4fd6858ca9))
- deps: Update Rust crates tracing-opentelemetry to 0.28 ([b1161bf](https://github.com/pando85/kaniop/commit/b1161bf23b38f4960742feb1e13aece4f57c8f86))
- deps: Update Rust crate serde_json to v1.0.133 ([33343c5](https://github.com/pando85/kaniop/commit/33343c53319f62ac374acc478200d625fc587538))
- deps: Update Rust crate anyhow to v1.0.93 ([a38568e](https://github.com/pando85/kaniop/commit/a38568eb84d96af5d197dd84c7376c14359ae8a4))
- deps: Update Rust crate thiserror to v1.0.69 ([e1d808d](https://github.com/pando85/kaniop/commit/e1d808d68ab3bd27482c21bfe8921ffea8065884))
- deps: Update Rust crate tempfile to v3.14.0 ([3319e26](https://github.com/pando85/kaniop/commit/3319e264b2c4df21baaabfff03315c32e69d67a3))
- deps: Update Rust crate hyper to v1.5.1 ([e1f9b38](https://github.com/pando85/kaniop/commit/e1f9b3863d73240b713af379e083b5503785ef9e))
- deps: Update Rust crate axum to 0.7 ([b90b5b4](https://github.com/pando85/kaniop/commit/b90b5b4e069918c9f83769851071a515a8bd78b8))
- deps: Update Rust crate thiserror to v2 ([a67fbfc](https://github.com/pando85/kaniop/commit/a67fbfcd02c0dfe8ab872a96cf197d608d2f21c3))
- deps: Update Rust crate kube to 0.97 ([94eaf9b](https://github.com/pando85/kaniop/commit/94eaf9bf4c60ab99cc813a477c4781069c757121))
- deps: Update Rust crate kanidm_client to v1.4.3 ([0389547](https://github.com/pando85/kaniop/commit/03895474f3945dcb782e40ba6034d3dbff9b7a28))
- deps: Update pre-commit hook gruntwork-io/pre-commit to v0.1.24 ([f68ccde](https://github.com/pando85/kaniop/commit/f68ccdebdba852d47c910ea769e663f3019c7b7a))
- deps: Update pre-commit hook alessandrojcm/commitlint-pre-commit-hook to v9.18.0 ([11fc846](https://github.com/pando85/kaniop/commit/11fc84613c3fb15f14a3153332358d108fbdf8bd))
- deps: Update pre-commit hook adrienverge/yamllint to v1.35.1 ([df2e076](https://github.com/pando85/kaniop/commit/df2e07655e0cd19a7a2c8769336f78522a0cf345))
- deps: Update pre-commit hook pre-commit/pre-commit-hooks to v4.6.0 ([b999a53](https://github.com/pando85/kaniop/commit/b999a53d674ac2a73cf7b609f92e32b38936337a))
- deps: Update pre-commit hook renovatebot/pre-commit-hooks to v39.26.3 ([cbe5757](https://github.com/pando85/kaniop/commit/cbe5757c92e3d7d0aea109049b80fde0ba061099))
- deps: Update pre-commit hook pre-commit/pre-commit-hooks to v5 ([ea15beb](https://github.com/pando85/kaniop/commit/ea15bebff3626bdc3643cbe94624d56de2b135e8))
- deps: Update pre-commit hook renovatebot/pre-commit-hooks to v39.27.0 ([fd58b22](https://github.com/pando85/kaniop/commit/fd58b228d98e56265d864efec4b2c538a26d54a8))
- deps: Update pre-commit hook renovatebot/pre-commit-hooks to v39.28.0 ([b8fd538](https://github.com/pando85/kaniop/commit/b8fd53816c4cac4332a732cffa9ee59da9d9ccef))
- deps: Update pre-commit hook renovatebot/pre-commit-hooks to v39.29.0 ([3d5ace6](https://github.com/pando85/kaniop/commit/3d5ace6130bf6baea8fba3a0bbc74dc09e9ec887))
- deps: Update pre-commit hook renovatebot/pre-commit-hooks to v39.30.0 ([ec1a274](https://github.com/pando85/kaniop/commit/ec1a274bebf4e75d4eccee8e9a8b3338ee34a9c9))
- deps: Update pre-commit hook renovatebot/pre-commit-hooks to v39.31.2 ([b9c1ca2](https://github.com/pando85/kaniop/commit/b9c1ca23259fa158fcbbcaa23ba77e6446aec43f))
- deps: Update pre-commit hook renovatebot/pre-commit-hooks to v39.31.3 ([8b5d6ec](https://github.com/pando85/kaniop/commit/8b5d6ec8e7b81cece128424e74fb522767668e75))
- deps: Update pre-commit hook renovatebot/pre-commit-hooks to v39.32.0 ([1a74506](https://github.com/pando85/kaniop/commit/1a74506eb3de60dc9859bf7991d41e32b095ff6b))
- deps: Update Rust crate tracing to v0.1.41 ([f1c0fc4](https://github.com/pando85/kaniop/commit/f1c0fc4d225231fb5f99de49676ca7d6080fd568))
- deps: Update pre-commit hook alessandrojcm/commitlint-pre-commit-hook to v9.19.0 ([805cb54](https://github.com/pando85/kaniop/commit/805cb5452f5d7ba0bdce56d0304b62d6e864f271))
- deps: Update Rust crate tracing-subscriber to v0.3.19 ([3f0aec4](https://github.com/pando85/kaniop/commit/3f0aec4f6b57dd80cc7a112b1b2ab8da5a9769ce))
- deps: Update pre-commit hook renovatebot/pre-commit-hooks to v39.42.4 ([5acde0b](https://github.com/pando85/kaniop/commit/5acde0b089495355f42337f18826d7c676952744))
- deps: Update opentelemetry-rust monorepo to v0.27.1 ([e47ecf3](https://github.com/pando85/kaniop/commit/e47ecf31476a8e2afbcf150d35ec974bbdc4ee5e))
- deps: Update Rust crate hostname to 0.4 ([f895c2f](https://github.com/pando85/kaniop/commit/f895c2fa4fd33e24465a9e455c3f2d2cfa79907e))
- deps: Update Rust crate thiserror to v2.0.4 ([76d7bdd](https://github.com/pando85/kaniop/commit/76d7bdde75f63322ee354310678c0dbb7e438131))
- deps: Update Rust crate tokio to v1.42.0 ([2a427c3](https://github.com/pando85/kaniop/commit/2a427c395834b73c2bbacd202aa4506a4b1b0469))
- deps: Update Rust crate time to v0.3.37 ([f8538cc](https://github.com/pando85/kaniop/commit/f8538cce2d9e0d4b692180c1fdea93ef16acb9b9))
- deps: Update Rust crate anyhow to v1.0.94 ([93ae9f1](https://github.com/pando85/kaniop/commit/93ae9f1cb1c52b016fa4561bdc62d23dc3f7c8a7))
- deps: Update Rust crate clap to v4.5.22 ([40b00aa](https://github.com/pando85/kaniop/commit/40b00aa5fd7225206c438df030fe0593fc74159c))
- deps: Update Rust crate http to v1.2.0 ([9aeee0f](https://github.com/pando85/kaniop/commit/9aeee0fb853e7bfca8ea269225f66b9b3d7afcc3))
- deps: Update Rust crate tokio-util to v0.7.13 ([10c0de5](https://github.com/pando85/kaniop/commit/10c0de51d443bc82cddfee86f9f628e5bc62f75b))
- deps: Update Rust crate clap to v4.5.23 ([129bc95](https://github.com/pando85/kaniop/commit/129bc956ae3ea18c9330abcb514edb1708cc1281))
- deps: Update Rust crate thiserror to v2.0.5 ([6e9f5c6](https://github.com/pando85/kaniop/commit/6e9f5c667109b1b64bb6240bd293efb540fd6538))
- deps: Update Rust crate thiserror to v2.0.6 ([9d3fa6a](https://github.com/pando85/kaniop/commit/9d3fa6ad695733e5faf9d0c7b39e1f8eb1a16cbe))
- deps: Update Rust crate chrono to v0.4.39 ([1f809d3](https://github.com/pando85/kaniop/commit/1f809d3b7345a46f3bf1f995d86bfe584e391ceb))
- deps: Update Rust crate serde to v1.0.216 ([14008fc](https://github.com/pando85/kaniop/commit/14008fce7290b47ded32b39a8b54308ea5f64a6f))
- deps: Update Rust crate tower to v0.5.2 ([e51f43e](https://github.com/pando85/kaniop/commit/e51f43ec2431fc40bab517fb2a56c2be97f61e07))
- deps: Update Rust crate thiserror to v2.0.7 ([86b016c](https://github.com/pando85/kaniop/commit/86b016cb511601088badc7226ca442acf699e710))
- deps: Update pre-commit hook gruntwork-io/pre-commit to v0.1.25 ([9044470](https://github.com/pando85/kaniop/commit/9044470ee588f9a0aeaa182674f5f1c53359bd42))
- deps: Update Rust crate hyper to v1.5.2 ([fb8dde9](https://github.com/pando85/kaniop/commit/fb8dde927d9d82e535ae6cd88ec5a62bfbe2aa46))
- deps: Update Rust crate thiserror to v2.0.8 ([bf7dd34](https://github.com/pando85/kaniop/commit/bf7dd34d290878fa8d7124efac22c9855c5d7ad4))
- deps: Update helm/kind-action action to v1.11.0 ([8f99735](https://github.com/pando85/kaniop/commit/8f997350e898c50a8e96d9eaace2da02a86a9a1b))
- deps: Update pre-commit hook alessandrojcm/commitlint-pre-commit-hook to v9.20.0 ([5c9b2e7](https://github.com/pando85/kaniop/commit/5c9b2e7c3c0a03dde490b43c414c77ff05312f0d))
- deps: Update wagoid/commitlint-github-action action to v6.2.0 ([391fcc7](https://github.com/pando85/kaniop/commit/391fcc74f6e9594a005f7123e8fee8e3e7eaa2a1))
- deps: Update Rust crate kanidm_client to v1.4.5 ([6687c86](https://github.com/pando85/kaniop/commit/6687c86b81b4002baa409d1339d37e3ad26c24da))
- deps: Update Rust crate thiserror to v2.0.9 ([1bb753b](https://github.com/pando85/kaniop/commit/1bb753b48b21c4da8dd4ab01dae71eebf433dc65))
- deps: Update Rust crate serde_json to v1.0.134 ([2c7c143](https://github.com/pando85/kaniop/commit/2c7c143cafc56840ff9393496f7eee74af8c892b))
- deps: Update Rust crate anyhow to v1.0.95 ([eefac8a](https://github.com/pando85/kaniop/commit/eefac8a77284b7f44783605603ec97f7ee11cc61))
- deps: Update helm/kind-action action to v1.12.0 ([2f21488](https://github.com/pando85/kaniop/commit/2f214880c79afc695a4d4ad6dedecd6d24fccc75))
- deps: Update Rust crate kube to 0.98... ([7655521](https://github.com/pando85/kaniop/commit/7655521e36aff42e251de12c10a9afffb9df412b))
- deps: Update Rust crate serde to v1.0.217 ([1f4d9fa](https://github.com/pando85/kaniop/commit/1f4d9fa5887b231ca1f067b920151b3ef716e6d4))
- deps: Update Rust crate serde_json to v1.0.135 ([6121483](https://github.com/pando85/kaniop/commit/612148368a065fc4a9059d8c83e2e1c0766d6e3b))
- deps: Update Rust crate clap to v4.5.24 ([9f2934a](https://github.com/pando85/kaniop/commit/9f2934a1894869d4b2d9984475326aa58eeb4c8f))
- deps: Update Rust crate tokio to v1.43.0 ([a03fd44](https://github.com/pando85/kaniop/commit/a03fd4417b70559b86f609169d2bf2617b19d4e9))
- deps: Update Rust crate thiserror to v2.0.10 ([827c12b](https://github.com/pando85/kaniop/commit/827c12b94daa4c189b1b13887473942da7831a49))
- deps: Update Rust crate tempfile to v3.15.0 ([5009416](https://github.com/pando85/kaniop/commit/5009416366581b1c5eb7abc4054f03d76c932287))
- deps: Update Rust crate prometheus-client to 0.23.0 ([954e9e2](https://github.com/pando85/kaniop/commit/954e9e2124175ef6ed2423f6f8e96fcf77b1918f))
- deps: Update Rust crate clap to v4.5.28 ([9ca5bf7](https://github.com/pando85/kaniop/commit/9ca5bf73e5a5bf29b8e83ef5da18a7e0687eefc2))
- deps: Update Rust crate serde_json to v1.0.138 ([c8e9247](https://github.com/pando85/kaniop/commit/c8e92471e9ca77c311cda3edc6287ddfc1ab22d4))
- deps: Update Rust crate thiserror to v2.0.11 ([b70798c](https://github.com/pando85/kaniop/commit/b70798c246ebfa9f326488e9f3e284fb193df94a))
- deps: Update wagoid/commitlint-github-action action to v6.2.1 ([1468ecc](https://github.com/pando85/kaniop/commit/1468eccac135fd6d267c1a723a41c001fcff2561))
- deps: Update Rust crate openssl to v0.10.70 ([904f0e7](https://github.com/pando85/kaniop/commit/904f0e78b9132bbf2598326293592169b2148332))
- deps: Update Rust crate hyper to v1.6.0 ([b9b68e4](https://github.com/pando85/kaniop/commit/b9b68e4c3811b3e8e9f1e2a6656e5e4763c91ac4))
- deps: Update Rust crate testcontainers to v0.23.2 ([055fb44](https://github.com/pando85/kaniop/commit/055fb44b1d661a692cf319773aee1fb2346e67c3))
- deps: Update Rust crate tempfile to v3.16.0 ([3810006](https://github.com/pando85/kaniop/commit/3810006c0ea471d0c63fad9e906c0a0e0d50b805))
- deps: Update pre-commit hook renovatebot/pre-commit-hooks to v39.164.1 ([ebe2364](https://github.com/pando85/kaniop/commit/ebe2364eb6af1142564eb529612251b6f6177ed1))
- deps: Update clechasseur/rs-clippy-check action to v4 ([537c6d9](https://github.com/pando85/kaniop/commit/537c6d927fb4330af21cdce9f184f3661c77fc47))
- deps: Update Rust crate prometheus-client to v0.23.1 ([0d6849a](https://github.com/pando85/kaniop/commit/0d6849aec7d28690787f66ad773d1243244389f8))
- deps: Update Rust crate axum to 0.8 ([bedc6f1](https://github.com/pando85/kaniop/commit/bedc6f175f6bd803093339652826869e379741b9))
- deps: Update ghcr.io/rash-sh/rash Docker tag to v2.9.1 ([51d8380](https://github.com/pando85/kaniop/commit/51d838020064a7bc90195fa9c08a0a59bb6c7340))
- deps: Update Rust crate kanidm_client to v1.5.0 ([84eb1e3](https://github.com/pando85/kaniop/commit/84eb1e3da9d5c0a52a613164c2c87412768376b6))
- deps: Update Rust crate time to v0.3.37 ([808fbd8](https://github.com/pando85/kaniop/commit/808fbd8806ef5c2c5608ec4c2eef8910be2a4e62))
- deps: Update Rust crate clap to v4.5.29 ([3bb69e6](https://github.com/pando85/kaniop/commit/3bb69e62f522e4f811d143170aecfeb11aec3e8e))
- deps: Update opentelemetry-rust monorepo to 0.28 ([4e54e17](https://github.com/pando85/kaniop/commit/4e54e17dfbd3a9b5741e1b9b2e67b72f2f663ae7))
- deps: Update Rust crate tracing-opentelemetry to 0.29 ([a747e03](https://github.com/pando85/kaniop/commit/a747e03b7bafb379cbcca66fa33868f1007fc9ee))
- deps: Update Rust crate tempfile to v3.17.1 ([f7cc36a](https://github.com/pando85/kaniop/commit/f7cc36ab67169d0cd29c11c225beed3064076d3d))
- deps: Update Rust crate clap to v4.5.30 ([aee768f](https://github.com/pando85/kaniop/commit/aee768f8fa7e531a2a2f5ff57096d534ea5ca961))
- deps: Update pre-commit hook alessandrojcm/commitlint-pre-commit-hook to v9.21.0 ([ecb707e](https://github.com/pando85/kaniop/commit/ecb707e593e1d7b71964ace7fafcabb944089f1e))
- deps: Update Rust crate openssl to v0.10.71 ([ff4969b](https://github.com/pando85/kaniop/commit/ff4969be5f35422843b8a271b2345bc968fc113e))
- deps: Update azure/setup-helm action to v4.3.0 ([e18460d](https://github.com/pando85/kaniop/commit/e18460d1ee8948bc5bdc0f60d732698e6038e93c))
- deps: Update Rust crate backon to v1.4.0 ([2bd8696](https://github.com/pando85/kaniop/commit/2bd8696d8ee06e176dc74016df9fdcdb4042372d))
- deps: Update Rust crate serde to v1.0.218 ([547b60a](https://github.com/pando85/kaniop/commit/547b60a6bfd11083c7aa6cfb4e4adcade722bb18))
- deps: Update Rust crate anyhow to v1.0.96 ([1a1db1a](https://github.com/pando85/kaniop/commit/1a1db1ace35a5dc360cc11823ee351f44a8128d1))
- deps: Update Rust crate serde_json to v1.0.139 ([42b2262](https://github.com/pando85/kaniop/commit/42b2262e8ed77dc3b25c85ce2e5c2c2acb9b1544))
- deps: Update pre-commit hook gruntwork-io/pre-commit to v0.1.26 ([1e7e835](https://github.com/pando85/kaniop/commit/1e7e835233c501de165feec98eb933133ae494b4))
- deps: Update Rust crate clap to v4.5.31 ([24241c7](https://github.com/pando85/kaniop/commit/24241c789d1fb0a78bd753dfef4adccb82917994))
- deps: Update Rust crate schemars to v0.8.22 ([36366e6](https://github.com/pando85/kaniop/commit/36366e6785df800026eb2099adf3b56b61aed199))
- deps: Update Rust crate chrono to v0.4.40 ([b00fbc1](https://github.com/pando85/kaniop/commit/b00fbc146d0a86063a234a465818b87fbb07fb74))
- deps: Update Rust crate testcontainers to v0.23.3 ([1d54670](https://github.com/pando85/kaniop/commit/1d546703ce2f1ebbc12e3f2e1a7a2b448c8047c0))
- deps: Update Rust crate json-patch to v4 ([3f0ee74](https://github.com/pando85/kaniop/commit/3f0ee7476d49a5ad52940c9bdc08dfe313955992))
- deps: Update pre-commit hook renovatebot/pre-commit-hooks to v39.182.3 ([b27fe0a](https://github.com/pando85/kaniop/commit/b27fe0a74e994d654513387ee2a082a675ce9f3f))
- deps: Update Rust crate anyhow to v1.0.97 ([bab2644](https://github.com/pando85/kaniop/commit/bab2644f29e61f0fab5f5fbf6b9d900412ec87d5))
- deps: Update Rust crate thiserror to v2.0.12 ([0fc065b](https://github.com/pando85/kaniop/commit/0fc065bc0ff5c41707b8b84d67abed528735b8c6))
- deps: Update Rust crate tokio to v1.44.0 ([b76784a](https://github.com/pando85/kaniop/commit/b76784a4b30dcb38ca53c0cc15e768c3d9ae1f6b))
- deps: Update Rust crate tempfile to v3.18.0 ([057439d](https://github.com/pando85/kaniop/commit/057439d3aa54e5912eaa8da10da3b21ada1761f9))
- deps: Update Rust crate time to v0.3.39 ([adb1e96](https://github.com/pando85/kaniop/commit/adb1e966348449280ea53437bdc271b8b62345ee))
- deps: Update Rust crate serde_json to v1.0.140 ([2ba7736](https://github.com/pando85/kaniop/commit/2ba7736efc5fcb0a2d869d683304350313700859))
- deps: Update Rust crate serde to v1.0.219 ([2b5b292](https://github.com/pando85/kaniop/commit/2b5b292342f2950e7b979d1c3432d80d51df9eca))
- deps: Update Rust crate clap to v4.5.32 ([bd3f546](https://github.com/pando85/kaniop/commit/bd3f54600c323c0967501576ae9f263689a07548))
- deps: Update Rust crate tokio to v1.44.1 ([d5fd593](https://github.com/pando85/kaniop/commit/d5fd593226f75238290e0d4599be8edbcf0a70d5))
- deps: Update Rust crate tempfile to v3.19.0 ([dcb88f5](https://github.com/pando85/kaniop/commit/dcb88f5e5cbad17a661a5eb9a12ca290343d310f))
- deps: Update Rust crate tokio-util to v0.7.14 ([24feec1](https://github.com/pando85/kaniop/commit/24feec1bffcb72904dc6315877cf99a618ecdc04))
- deps: Update Rust crate kube to 0.99 ([a5a3460](https://github.com/pando85/kaniop/commit/a5a34605b60eebd99123986d82699978f007aedd))
- deps: Update Rust crate http to v1.3.1 ([7296873](https://github.com/pando85/kaniop/commit/7296873d2a932b71f41c4288ad7b58b0a5fcb8cf))
- deps: Update pre-commit hook alessandrojcm/commitlint-pre-commit-hook to v9.22.0 ([4d99a93](https://github.com/pando85/kaniop/commit/4d99a9391003ab9dac0c8a9e57ded8ea32f5bb4b))
- deps: Update pre-commit hook adrienverge/yamllint to v1.36.0 ([e7ae3f0](https://github.com/pando85/kaniop/commit/e7ae3f0a8c96c9cb4cdb511b89917f9a9108e374))
- deps: Update pre-commit hook adrienverge/yamllint to v1.36.1 ([e186282](https://github.com/pando85/kaniop/commit/e18628217382ae92d0d18eee1520dbdd3752bd96))
- deps: Update pre-commit hook adrienverge/yamllint to v1.36.2 ([8de4d4e](https://github.com/pando85/kaniop/commit/8de4d4eeaf747ee096108aa92bd811dcce9d8074))
- deps: Update ghcr.io/rash-sh/rash Docker tag to v2.9.2 ([86a7552](https://github.com/pando85/kaniop/commit/86a7552c068dbddb40ade5ba39b39adb71758e4b))
- deps: Update Rust crate backon to v1.4.1 ([64361fd](https://github.com/pando85/kaniop/commit/64361fdf4da342b04b439d8743ab514a04f3387a))
- deps: Update Rust crate tempfile to v3.19.1 ([70af8c3](https://github.com/pando85/kaniop/commit/70af8c37b15c5a04e2afd8e5c302d8aacc03e7b2))
- deps: Update ghcr.io/rash-sh/rash Docker tag to v2.9.3 ([86f0ccb](https://github.com/pando85/kaniop/commit/86f0ccb2fd415cfce0f1cec83f904f94d9a76161))
- deps: Update pre-commit hook gruntwork-io/pre-commit to v0.1.28 ([0313762](https://github.com/pando85/kaniop/commit/0313762f52da34c034c25bc5f65b18bd5883659c))
- deps: Update ghcr.io/rash-sh/rash Docker tag to v2.9.4 ([6daf729](https://github.com/pando85/kaniop/commit/6daf729a8231b1a0e36d08b39d006b20a318f42b))
- deps: Update ghcr.io/rash-sh/rash Docker tag to v2.9.5 ([c141b5e](https://github.com/pando85/kaniop/commit/c141b5e5bb07e52273265799b9e8c319ff4e126d))
- deps: Update ghcr.io/rash-sh/rash Docker tag to v2.9.6 ([0c93af3](https://github.com/pando85/kaniop/commit/0c93af309d00bbf10a870479c893dcde0e4d49fa))
- deps: Update Rust crate time to v0.3.41 ([0d535a6](https://github.com/pando85/kaniop/commit/0d535a68b52abb3531b24d631e79bea831bbbcf7))
- deps: Update pre-commit hook adrienverge/yamllint to v1.37.0 ([fd9d7a3](https://github.com/pando85/kaniop/commit/fd9d7a38932b67d4de296173c7ef5f2641b8d572))
- deps: Update Rust crate tracing-opentelemetry to 0.30 ([6c6d81a](https://github.com/pando85/kaniop/commit/6c6d81a3be17e014dc81728979b700d817dc1a58))
- deps: Update opentelemetry-rust monorepo to 0.29 ([d544a9d](https://github.com/pando85/kaniop/commit/d544a9d75bf792a7a0c5011a7c20ad39ba1f32dd))
- deps: Update Rust crate clap to v4.5.33 ([5b7d757](https://github.com/pando85/kaniop/commit/5b7d75752691d2b66618625a61add7e210d90db0))
- deps: Update Rust crate tonic to 0.13 ([0bfc594](https://github.com/pando85/kaniop/commit/0bfc594c4dccd99baf4596d211844f237f9575ef))
- deps: Update Rust crate clap to v4.5.34 ([2f110f0](https://github.com/pando85/kaniop/commit/2f110f0dc30350162d3b3d5de9159a472cc8491a))
- deps: Update ghcr.io/rash-sh/rash Docker tag to v2.9.7 ([8577455](https://github.com/pando85/kaniop/commit/85774557113d173ff4bc25368dddaebb9f3ac26f))
- deps: Update Rust crate hyper-util to v0.1.11 ([4354b93](https://github.com/pando85/kaniop/commit/4354b934503acdf43737eea24d8c48a0fcf2b61e))
- deps: Update Rust crate clap to v4.5.35 ([18fa30b](https://github.com/pando85/kaniop/commit/18fa30b00d5eeb82256800df1796bd1edcfb3bd0))
- deps: Update Rust crate axum to v0.8.3 ([e2e91da](https://github.com/pando85/kaniop/commit/e2e91da8764074de48458a503ea398a5a3eee199))
- deps: Update pre-commit hook renovatebot/pre-commit-hooks to v39.227.2 ([34659a6](https://github.com/pando85/kaniop/commit/34659a695e4b0bb05126164088c338e185c83dfc))
- deps: Update ghcr.io/rash-sh/rash Docker tag to v2.9.9 ([63cacfd](https://github.com/pando85/kaniop/commit/63cacfdf72952934a2c8463e349b4364ec0bc7d8))
- deps: Update Rust crate opentelemetry to v0.29.1 ([d6fbe99](https://github.com/pando85/kaniop/commit/d6fbe9975904c5406da2bc56ef75f2f69a7213db))
- deps: Update Kubernetes version to v1.32.3 ([aadd995](https://github.com/pando85/kaniop/commit/aadd995e31d07394e9f83c1a34c891d07411b17c))
- deps: Update Rust crate openssl to v0.10.72 ([eff5a33](https://github.com/pando85/kaniop/commit/eff5a3331f0567bd6cf2eec8936a228146f35f3a))
- deps: Update Rust crate tokio to v1.44.2 ([81495e0](https://github.com/pando85/kaniop/commit/81495e0fd1efc0c7b71abb1dd3529abd3803f88e))
- deps: Update Rust crate backon to v1.5.0 ([ced274b](https://github.com/pando85/kaniop/commit/ced274b21e3a4e01c724a12eb709ce78d3b54b83))
- deps: Update Rust crate clap to v4.5.36 ([eb86a6d](https://github.com/pando85/kaniop/commit/eb86a6d04975919072655f76322b30cfecf49c4f))
- deps: Update Rust crate anyhow to v1.0.98 ([9e265a8](https://github.com/pando85/kaniop/commit/9e265a898477a700a35285890c012c647a0091db))
- deps: Update Rust crate clap to v4.5.37 ([35f3955](https://github.com/pando85/kaniop/commit/35f39551d5900dc3dbdee39469b0186ff3a7dbe1))
- deps: Update pre-commit hook gruntwork-io/pre-commit to v0.1.29 ([1beaa43](https://github.com/pando85/kaniop/commit/1beaa43167c6231421382af203732af091c13bca))
- deps: Update Rust crate tokio-util to v0.7.15 ([4b490a6](https://github.com/pando85/kaniop/commit/4b490a6c2569f609c9ff9432f36d962a1e419c06))
- deps: Update ghcr.io/rash-sh/rash Docker tag to v2.9.10 ([ba7b135](https://github.com/pando85/kaniop/commit/ba7b13584ed680d9a2f40e52fea4d84b56ee4396))
- deps: Update Rust crate chrono to v0.4.41 ([16848ba](https://github.com/pando85/kaniop/commit/16848ba6f3934ba5161e6897b82eb6aeca5f264d))
- deps: Update ghcr.io/rash-sh/rash Docker tag to v2.9.11 ([f08062d](https://github.com/pando85/kaniop/commit/f08062dca068cf336eb1e3661b528feb11f0729b))
- deps: Update Rust crate axum to v0.8.4 ([8f95ad7](https://github.com/pando85/kaniop/commit/8f95ad7d1e63fb25ecd47c75f5aa3087aab1f3ea))
- deps: Update pre-commit hook renovatebot/pre-commit-hooks to v40 ([b5dbfd6](https://github.com/pando85/kaniop/commit/b5dbfd66669273f3015f009713345a2c1f7cb850))
- deps: Update Rust crate tonic to v0.13.1 ([f028e59](https://github.com/pando85/kaniop/commit/f028e59c5256504e8ceb0702c39e997d56a33484))
- deps: Update pre-commit hook adrienverge/yamllint to v1.37.1 ([421162d](https://github.com/pando85/kaniop/commit/421162d1c752f19934205e2730ba4b57cba9d03b))
- deps: Update Rust crate tokio to v1.45.0 ([d69b397](https://github.com/pando85/kaniop/commit/d69b39799523c2286e8a690cfc68a63d843a3981))
- deps: Update Rust crate testcontainers to 0.24 ([da9a3c2](https://github.com/pando85/kaniop/commit/da9a3c2d35146197399ed5ca9c2da7e6a1bc7c79))
- deps: Update Kanidm to 1.6.2 ([7ebb843](https://github.com/pando85/kaniop/commit/7ebb843c8c85e6fa537de6ecf790531782106eb2))
- deps: Update Rust crate clap to v4.5.38 ([8f84f28](https://github.com/pando85/kaniop/commit/8f84f288f3ef00883cb551c60bfaed23cb158ddf))
- deps: Update Rust crate tempfile to v3.20.0 ([7fd6383](https://github.com/pando85/kaniop/commit/7fd63834178b840644b8a4ba4bbb72a76160b94d))
- deps: Update Rust crate kanidm_client to v1.6.3 ([5d1eff9](https://github.com/pando85/kaniop/commit/5d1eff9ff4979986fdb40cbbed00c918af117d5d))
- deps: Update Rust crate kube to v1 ([d6fb50e](https://github.com/pando85/kaniop/commit/d6fb50e87de1075a839ea443f282f6e5ab0d6f47))
- deps: Update Rust crate k8s-openapi to v0.25 ([5452cae](https://github.com/pando85/kaniop/commit/5452caecb39ff9e6b89f319d30b597272d054a77))
- deps: Update Rust crate hyper-util to v0.1.12 ([1891ccb](https://github.com/pando85/kaniop/commit/1891ccb560184017a36f650ad969426b1ad237a0))
- deps: Update dependency kubernetes-sigs/kind to v0.29.0 ([4f3c70b](https://github.com/pando85/kaniop/commit/4f3c70b4af60e97a90569f50afc507fe2d86d890))
- deps: Update Rust crate tokio to v1.45.1 ([91c98c9](https://github.com/pando85/kaniop/commit/91c98c996f377f498310def870f352a8a5add87e))
- deps: Update Rust crate kube to v1.1.0 ([b2528eb](https://github.com/pando85/kaniop/commit/b2528eb9cf76ff19d8d69bec095018cea768fc92))
- deps: Update Rust crate clap to v4.5.39 ([a860c5d](https://github.com/pando85/kaniop/commit/a860c5dbeba6ab20cea641e7d5cc65d62e9cf4ad))
- deps: Update Rust crate hyper-util to v0.1.13 ([6a7e4f0](https://github.com/pando85/kaniop/commit/6a7e4f0958fe09eaefb989c610c23be7d67b4f32))
- deps: Update Rust crate openssl to v0.10.73 ([78a53f1](https://github.com/pando85/kaniop/commit/78a53f1e3f641998df553de79292c6afb07cbd44))
- deps: Update Rust crate backon to v1.5.1 ([61cd5db](https://github.com/pando85/kaniop/commit/61cd5db55597e1525fe0864a9660715b87b40b96))
- deps: Update Rust crate hyper-util to v0.1.14 ([cde44e4](https://github.com/pando85/kaniop/commit/cde44e4e893c40588f4771aed0c489a5b34c0ce3))
- deps: Update pre-commit hook renovatebot/pre-commit-hooks to v40.48.3 ([f33f553](https://github.com/pando85/kaniop/commit/f33f55350982b8871e7fda55f382b6193bb59f74))
- deps: Update ghcr.io/rash-sh/rash Docker tag to v2.9.12 ([324c601](https://github.com/pando85/kaniop/commit/324c60118755f95fd4d059e3dc337efabde233fc))
- deps: Update Rust crate tracing-opentelemetry to 0.31 ([fb57da7](https://github.com/pando85/kaniop/commit/fb57da743b7d238e62c10a2807faa6e6b61c5fbf))
- deps: Update Rust crate opentelemetry to 0.30 ([a040f34](https://github.com/pando85/kaniop/commit/a040f34097500369246b9a53c63ec465678508c2))
- deps: Update Rust crate clap to v4.5.40 ([ec81635](https://github.com/pando85/kaniop/commit/ec8163593765cf92f6dd43e8f1aba26aa96f2189))
- deps: Update Rust crate kanidm_client to v1.6.4 ([32fcf6c](https://github.com/pando85/kaniop/commit/32fcf6c1c3df80799e8a3d7fb43a8b080663e061))
- deps: Update ghcr.io/rash-sh/rash Docker tag to v2.14.2 ([a1d21db](https://github.com/pando85/kaniop/commit/a1d21dbf5f1aad2be92d6acb818582b89c39298d))
- deps: Update ghcr.io/rash-sh/rash Docker tag to v2.15.0 ([dcb4ae3](https://github.com/pando85/kaniop/commit/dcb4ae35ed76ebf3d55d30a14b01d5255d55efed))
- deps: Update ghcr.io/rash-sh/rash Docker tag to v2.16.0 ([85a0786](https://github.com/pando85/kaniop/commit/85a0786aa7bf4ca4d4e9111feee3000fb26096cd))
- deps: Update ghcr.io/rash-sh/rash Docker tag to v2.16.1 ([409612f](https://github.com/pando85/kaniop/commit/409612f9500d7d04ad57a73b2e459ebf48d7cde6))
- deps: Update pre-commit hook renovatebot/pre-commit-hooks to v40.62.1 ([cc7e32a](https://github.com/pando85/kaniop/commit/cc7e32a9e53bf97fe890d83c8a542d3a845f94b1))
- deps: Update pre-commit hook renovatebot/pre-commit-hooks to v41 ([45d185c](https://github.com/pando85/kaniop/commit/45d185c8d3d21f76f6a3bb60a2b45503272af83f))
- deps: Update pre-commit hook gruntwork-io/pre-commit to v0.1.30 ([0f86684](https://github.com/pando85/kaniop/commit/0f8668455e3e26a98653541338579c5773fcd453))
- deps: Update Rust crate clap to v4.5.41 ([21764dd](https://github.com/pando85/kaniop/commit/21764dd1c2de3c35aa8a269161171827ce5ec778))
- deps: Update Rust crate serde_json to v1.0.141 ([b80a421](https://github.com/pando85/kaniop/commit/b80a4212627b35f785fcb66e1e4f01662c44a00d))
- deps: Update Rust crate hyper-util to v0.1.16 ([bf77fbf](https://github.com/pando85/kaniop/commit/bf77fbf258056138e410bb39b87ca31be13cde50))
- deps: Update Rust crate tokio to v1.46.1 ([b4668a7](https://github.com/pando85/kaniop/commit/b4668a748d90891dbcea5eed609a736ee159c41c))
- deps: Update appany/helm-oci-chart-releaser action to v0.5.0 ([335d4aa](https://github.com/pando85/kaniop/commit/335d4aabd1d98dd44cbf3fd5d893b34e431f5a0b))
- deps: Update Rust crate testcontainers to 0.25 ([e17cdbd](https://github.com/pando85/kaniop/commit/e17cdbde3834d72e093c074d3bb87342ac486495))
- deps: Update Rust crate tokio to v1.47.0 ([cd2e78c](https://github.com/pando85/kaniop/commit/cd2e78c79d78b73c6c25284e8a9edda225204f92))
- deps: Update Rust crate clap to v4.5.42 ([ba5bb03](https://github.com/pando85/kaniop/commit/ba5bb038f26844a24d2e0f97687f7f9e766504e1))
- deps: Update Rust crate backon to v1.5.2 ([e847694](https://github.com/pando85/kaniop/commit/e84769457e1b2bce30ce8b5e5c1a712380946148))
- deps: Update Rust crate serde_json to v1.0.142 ([98725d6](https://github.com/pando85/kaniop/commit/98725d6897a10ed7301a980924ecc40480ee6b86))
- deps: Update pre-commit hook renovatebot/pre-commit-hooks to v41.43.0 ([5fd2d42](https://github.com/pando85/kaniop/commit/5fd2d42b6257e2fb0157f14bd11c78987e2dd51e))
- deps: Update Rust crate tokio to v1.47.1 ([9180e75](https://github.com/pando85/kaniop/commit/9180e75f0dba51f190f5e19193d33f6ed0710463))
- deps: Update Rust crate clap to v4.5.43 ([1906e85](https://github.com/pando85/kaniop/commit/1906e85b1e135be7b73d45ac21366ce99dbe539c))
- deps: Update Rust crate clap to v4.5.44 ([4256ce0](https://github.com/pando85/kaniop/commit/4256ce0e09ece3c033c1353fcc170b7151bc59ad))
- deps: Update Rust crate thiserror to v2.0.13 ([a589239](https://github.com/pando85/kaniop/commit/a5892396414ac0336ec66e035c0a7cd056fc9d4d))
- deps: Update actions/checkout action to v5 ([b64b930](https://github.com/pando85/kaniop/commit/b64b930bd67c161ff02d35665bd6019d8f958802))
- deps: Update pre-commit hook pre-commit/pre-commit-hooks to v6 ([a577266](https://github.com/pando85/kaniop/commit/a5772662f76783fc0560e41bb04dec05932bd760))
- deps: Update Rust crate tokio-util to v0.7.16 ([9829965](https://github.com/pando85/kaniop/commit/9829965fe00317d6085fa050e0c125f9b0b72794))
- deps: Update Rust crate anyhow to v1.0.99 ([dff57a8](https://github.com/pando85/kaniop/commit/dff57a802a5fd8e958372bdb37226f4553b80a52))
- deps: Update Rust crate thiserror to v2.0.14 ([ad1ea06](https://github.com/pando85/kaniop/commit/ad1ea06e2fc35fd8bfd14e35a8755813fdc9bf78))
- deps: Update Rust crate clap to v4.5.45 ([c4a6d1f](https://github.com/pando85/kaniop/commit/c4a6d1f1940d8bfa8408c1580f87105dee266b7c))
- deps: Update Rust crate url to v2.5.6 ([519975c](https://github.com/pando85/kaniop/commit/519975c98ab8d89a47fc79c64786a12da0c82592))
- deps: Update azure/setup-helm action to v4.3.1 ([dd099f5](https://github.com/pando85/kaniop/commit/dd099f5185362735c04ec97309a17f8813ad37bd))
- deps: Update Rust crate serde_json to v1.0.143 ([9d4eb07](https://github.com/pando85/kaniop/commit/9d4eb074e6f2030c24e9c0fe09b8d9d4baa8691c))
- deps: Update Rust crate thiserror to v2.0.16 ([2bfc86f](https://github.com/pando85/kaniop/commit/2bfc86fb3d6063d456c509dfbd7262c1931a75a5))
- deps: Update Rust crate url to v2.5.7 ([0b6138f](https://github.com/pando85/kaniop/commit/0b6138f5de1b69c7a7e8509a82a82dc4f4cb605d))
- deps: Update Rust crate clap to v4.5.46 ([d191900](https://github.com/pando85/kaniop/commit/d19190043054e7cf8f8d38e5347e744e13eb7628))
- deps: Update Rust crate clap to v4.5.47 ([5138ea7](https://github.com/pando85/kaniop/commit/5138ea79fbf7b05698571df38de1270cc664665f))
- deps: Update Rust crate time to v0.3.43 ([df49996](https://github.com/pando85/kaniop/commit/df49996e42cc334d3dccca0556f456159c9de609))
- deps: Update actions/setup-python action to v6 ([5723e5d](https://github.com/pando85/kaniop/commit/5723e5df3bb211a9b5fd0f6e6351e95f467cb18e))
- deps: Update clechasseur/rs-clippy-check action to v5 ([bc0cd29](https://github.com/pando85/kaniop/commit/bc0cd2938002f87de4d8b77949f085d01c88df9d))
- deps: Update Rust crate tempfile to v3.22.0 ([d694d2a](https://github.com/pando85/kaniop/commit/d694d2a571d37dd56ea6904235d1d7df6b4049ae))
- deps: Update Rust crate chrono to v0.4.42 ([2e25d0f](https://github.com/pando85/kaniop/commit/2e25d0fb44884b9ec2427a7441a247a2e49d94ba))
- deps: Update Rust crate tracing-subscriber to v0.3.20 ([63b3f7d](https://github.com/pando85/kaniop/commit/63b3f7d94145d073324191ca63a4cf6d1b00d85b))
- deps: Update dependency kubernetes-sigs/kind to v0.30.0 ([7b4cd36](https://github.com/pando85/kaniop/commit/7b4cd3687f57b8b74ae4a501fa240a9cc8f602bc))
- deps: Update Rust crate kube to v2 ([b0bc2b4](https://github.com/pando85/kaniop/commit/b0bc2b4c1c266313e33e18bde4c21d5f9e5c5549))
- deps: Update Rust crate hyper to v1.7.0 ([5031dc6](https://github.com/pando85/kaniop/commit/5031dc60b0fe644ff91ff02bfcc6e712af00da79))
- deps: Update Rust crate prometheus-client to 0.24.0 ([ef9b0ec](https://github.com/pando85/kaniop/commit/ef9b0ececf78ba5fd5c476c3317a4d52144d67fa))
- deps: Update Rust crate tonic to 0.14 ([d716872](https://github.com/pando85/kaniop/commit/d716872d21e6987662bb78e019eafcb41c520320))
- deps: Update Rust crate kube to v2.0.1 ([d9a42ec](https://github.com/pando85/kaniop/commit/d9a42ec125fd30d341b205ebf011c814f6f656a2))
- deps: Update Rust crate serde_json to v1.0.144 ([3ed0767](https://github.com/pando85/kaniop/commit/3ed0767ebc09242da4e088341bc707d6494104c2))
- deps: Update Rust crate serde to v1.0.221 ([b818cde](https://github.com/pando85/kaniop/commit/b818cde8929cfce304bf2e8ad913ed257350f99e))
- deps: Update Rust crate serde_json to v1.0.145 ([a2ccf1d](https://github.com/pando85/kaniop/commit/a2ccf1d6abdfb03e06d694ba97de0a79e882dc58))
- deps: Update Rust crate serde to v1.0.223 ([fb11335](https://github.com/pando85/kaniop/commit/fb113351d6a71571a620e632c1d450103538d534))
- deps: Update Rust crate serde to v1.0.224 ([8b04f5f](https://github.com/pando85/kaniop/commit/8b04f5f9655c8a530e61d66d48247b63ee7f6de4))
- deps: Update Rust crate hyper-util to v0.1.17 ([99fd212](https://github.com/pando85/kaniop/commit/99fd2121e8222acfe0114a5b303d3736817f15e7))
- deps: Update Rust crate json-patch to v4.1.0 ([0da131d](https://github.com/pando85/kaniop/commit/0da131d68e2727c1da246d079628d0ac847d837e))
- deps: Update Rust crate serde to v1.0.225 ([4b49817](https://github.com/pando85/kaniop/commit/4b49817493b6f14b63436b64142410b1d0a294b3))
- deps: Update Rust crate anyhow to v1.0.100 ([e5ab71a](https://github.com/pando85/kaniop/commit/e5ab71a63fab667bdfc59e29f283ed74f1320e4d))
- deps: Update Rust crate clap to v4.5.48 ([07daa80](https://github.com/pando85/kaniop/commit/07daa80fd661fc8f912d9af18ebd23dca6109d21))
- deps: Update Rust crate serde to v1.0.226 ([59994aa](https://github.com/pando85/kaniop/commit/59994aad1f56d0140c25ab10c46923ea25f3bc1a))
- deps: Update Rust crate time to v0.3.44 ([e9804be](https://github.com/pando85/kaniop/commit/e9804be93a058d4afd6343369162a9fee75aa931))
- deps: Update Rust crate tempfile to v3.23.0 ([26cea62](https://github.com/pando85/kaniop/commit/26cea62f09b17791d87fc523d15f4208cc55320e))
- Add multi-arch docker build and releases ([028df4f](https://github.com/pando85/kaniop/commit/028df4f80e226de9e4a621848b810aca3215c02a))
- Change release `--frozen` by `--locked` ([3017953](https://github.com/pando85/kaniop/commit/30179532e2df42a70641b367463c53afb0530d8c))
- Add permissions for package write ([d04a08e](https://github.com/pando85/kaniop/commit/d04a08ef3bf90b1d2af2227546d12322e8d34609))
- Optimize release binary ([a39eeab](https://github.com/pando85/kaniop/commit/a39eeabb8d14ea118e18fe3551750ad4339bbf6f))
- Reduce to minimum dependencies ([f1fb047](https://github.com/pando85/kaniop/commit/f1fb0473e16056477900e3dd2c2ffe00d79b3795))
- Improv ecompile time ([2f84db7](https://github.com/pando85/kaniop/commit/2f84db796802798dd174a62956dcab0c6bb0e700))
- Remove deprecated NOTPARALLEL instruction ([6aa9db2](https://github.com/pando85/kaniop/commit/6aa9db2119354fd30091a0b4a246944172b507d9))
- Add openssl vendored to workaround kanidm cross compilation ([81661a9](https://github.com/pando85/kaniop/commit/81661a937761ed425523798ecdb80ce43a23944f))
- Update rust to 1.85.0 and rust edition to 2024 ([d7b1e94](https://github.com/pando85/kaniop/commit/d7b1e9445e172bd6b893da9db79ec1755fc3f6d8))
- Update cargo lock ([361560b](https://github.com/pando85/kaniop/commit/361560b0ac24fe5eecabaa94388fd7d2dc610400))

### Refactor

- ci: Migrate config renovate.json5 ([0a028f7](https://github.com/pando85/kaniop/commit/0a028f7fffd699acac7a215dfbbc7242936c9341))
- ci: Migrate config renovate.json5 ([32de94c](https://github.com/pando85/kaniop/commit/32de94c3c7a37fe4d6dfbbaca66455ebcda528f1))
- ci: Migrate config renovate.json5 ([88428e7](https://github.com/pando85/kaniop/commit/88428e78731a7baf2322f4b222ef838f2063676a))
- cmd: Replace actix with axum ([c1b3a02](https://github.com/pando85/kaniop/commit/c1b3a027cb608ce5fb77c94f82fbf9e86cf51238))
- deps: Upgrade schemars to v1.0 ([90d1e6e](https://github.com/pando85/kaniop/commit/90d1e6e49adfef1fde1cd4bec467107e4a5be208))
- deps: Move validations from admission policy to schemars ([9a5a95d](https://github.com/pando85/kaniop/commit/9a5a95dd489f2380a1728cd83c2fd312112d101d))
- kanidm: Move status to a different file ([2659fe4](https://github.com/pando85/kaniop/commit/2659fe410f6823c7fa17084d4a55cf3d45ebd469))
- kanidm: Change from deployment to statefulset ([ce1c586](https://github.com/pando85/kaniop/commit/ce1c586decedd9333d70d6135f5ba5d08b790826))
- kanidm: Simplify controller watchers and stores code ([e4767cd](https://github.com/pando85/kaniop/commit/e4767cd04e2065bc0af0ff25bf55d3df68acf315))
- kanidm: Break down statefulset creation into smaller functions ([5f000d1](https://github.com/pando85/kaniop/commit/5f000d11bb2886b62bc04c99c5467d9ef9420144))
- kanidm: Reduce exposure in SecretExt trait ([7d02946](https://github.com/pando85/kaniop/commit/7d0294644641b0415a54b1145ce9518e28dea394))
- operator: Add generic trait for patch and delete ([1ffaab8](https://github.com/pando85/kaniop/commit/1ffaab822908920c3c351b5c50b2f09c783ad8f4))
- Change structure for libs and cmd ([d9688f0](https://github.com/pando85/kaniop/commit/d9688f01dfa004121860c69341467f72639bd7ec))
- Add telemetry, axum and new dir structures ([770795e](https://github.com/pando85/kaniop/commit/770795ee8eab67d6751cc3a2ce6ffd1fb2031000))
- Move echoes controller to echo mod ([749b86b](https://github.com/pando85/kaniop/commit/749b86bfb7171ba25cd3c4bc665579b5be05ce7a))
- Remove diagnostics ([ae4d3d1](https://github.com/pando85/kaniop/commit/ae4d3d17111dcc61c800f874a854d097e2955243))
- Echo docs and minor code changes ([dcfa57f](https://github.com/pando85/kaniop/commit/dcfa57fbbbe5c143932ecd2cd45ae37a137dced3))
- Use kube-rs finalizers for handling reconcile events ([c5a0599](https://github.com/pando85/kaniop/commit/c5a059961e687ae31728efeafaee8fd2445aebe0))
- Replace `match` with `ok_or_else` and add explicit rustfmt config ([c17daa6](https://github.com/pando85/kaniop/commit/c17daa6e802c66e95e11278961d9eaab3d902d9e))
- Add features to workspace and integration-tests package ([037d631](https://github.com/pando85/kaniop/commit/037d631653595718ae4e60e26d1ff4339286e964))
- Add feature for integration tests and add e2e tests to makefile ([a644b47](https://github.com/pando85/kaniop/commit/a644b476dbe2d1a084d64d07a166dce6696e7759))
- Simplify e2e targets in Makefile ([20c4aa9](https://github.com/pando85/kaniop/commit/20c4aa9a581c6233a074ef20b80e6176cac45859))
- Rename tests to test ([4251af4](https://github.com/pando85/kaniop/commit/4251af41830bb51d5400c80f14684812c70cf2d4))
- Rename echo resource to kanidm ([da760c8](https://github.com/pando85/kaniop/commit/da760c86e2e00306abdf137378fbadd305a7ab02))
- Format json definitions ([c6ae33f](https://github.com/pando85/kaniop/commit/c6ae33f8c46615c94b06e44116782544bf4e972e))
- Use relative imports and split oauth2 reconcile ([5881e8e](https://github.com/pando85/kaniop/commit/5881e8e52fd79627ad0555f6792764ee0c1c9455))
- Move kanidm to its own module inside operator ([e2a2307](https://github.com/pando85/kaniop/commit/e2a2307c4b29316b05523816b71ca4b9b3f0050e))
- Remove namespace parameter from reconciles ([3e09d9d](https://github.com/pando85/kaniop/commit/3e09d9dd1b2294254038c3906226170cf582d544))
- Make e2e-tests configurable for any kubernetes version ([c68966b](https://github.com/pando85/kaniop/commit/c68966b6541a3d7a985f4afe77c43144d54e2e56))

### Testing

- ci: Disable integration test for arm64 ([e4753d2](https://github.com/pando85/kaniop/commit/e4753d22acac517001f2f57ea677d3dc79fa8b5d))
- ci: Limit e2e concurrence to 4 ([a9b9243](https://github.com/pando85/kaniop/commit/a9b92430887b038f3488b94468b98a047c365948))
- ci: Add pre-commit workflow and deprecate commitlint ([64a49e5](https://github.com/pando85/kaniop/commit/64a49e5679b695644c61d9dcf0e82c6e8a26bf1f))
- group: Fix `group_lifecycle` race condition on Posix attributes ([52c0ccb](https://github.com/pando85/kaniop/commit/52c0ccbeabea20b23dc3473939bf9d29b1a46fee))
- kanidm: Ensure replication is correctly configured in e2e checks ([bcf78bc](https://github.com/pando85/kaniop/commit/bcf78bcbf6a1ff307718d768f141399d9a8d83f2))
- kanidm: Fix naming resolution for external Kanidm pods ([23cbe2b](https://github.com/pando85/kaniop/commit/23cbe2b63889a1acb103a6f1bb3fac66adb9a2e8))
- kanidm: Change wait for replication time var in `kanidm_external_replication_node` ([c33a665](https://github.com/pando85/kaniop/commit/c33a6654913e9d20a4ee507b338c7eab7c56a1c7))
- Add unittests for Helm charts ([be49cae](https://github.com/pando85/kaniop/commit/be49cae8b18f7c0f31feb4278a90456370b73b37))
- Add reconcile unittests ([31faff9](https://github.com/pando85/kaniop/commit/31faff9e3387c267f2376b827744da0e9a0e1cdd))
- Increase timeout to 30s ([41410fd](https://github.com/pando85/kaniop/commit/41410fde2287908ea0a9f38aecfb922b41ffd4f2))
- Remove integration-tests ([168ae32](https://github.com/pando85/kaniop/commit/168ae32e72d24a77c476c15431774458b3b19df9))
- Force `image.tag` in e2e to be a string ([cd932d7](https://github.com/pando85/kaniop/commit/cd932d7a717976b4e0d49e71c4db4ee724422dee))
- Clean metadata fields before applying patch ([199b8e8](https://github.com/pando85/kaniop/commit/199b8e8b2582f2dc3fcc103726a25165ef39f3f4))
- Change backoff by backon crate in e2e ([8d2688e](https://github.com/pando85/kaniop/commit/8d2688e8fdbab35e3970d0b3eff161eb2a982b05))
- Remove person objects in clean-e2e removing finalizer ([c4299e7](https://github.com/pando85/kaniop/commit/c4299e72aad78a9e800a67a346793507550c63a4))
- Get Kanidm version from `Cargo.lock` instead of `Cargo.toml` ([532203c](https://github.com/pando85/kaniop/commit/532203c0a8647bc88043f1ae44041fe8179438ff))
- Add wait to event list in `person_attributes_collision` ([ac24a26](https://github.com/pando85/kaniop/commit/ac24a2643696236ceaee2d8128456e9d40768380))
- Ensure events are waited with `check_event_with_timeout` ([2b3df3b](https://github.com/pando85/kaniop/commit/2b3df3b976bea1be71c3a84ae5bcbb40f11b27f1))
- Show kaniop logs when e2e tests fail ([e54efb5](https://github.com/pando85/kaniop/commit/e54efb5a16254d1bf90f65d10145d50a2d32d362))
- Add debug commands to e2e tests ([5d4e3c7](https://github.com/pando85/kaniop/commit/5d4e3c7f225ca6458f4c103a41594e34348ec8bf))
- Upgrade kind to 0.27.0 ([c49254b](https://github.com/pando85/kaniop/commit/c49254bc305056b4d0432f9962d3e66ff7a8a2ec))
- Fast fail e2e if kaniop does not start ([4dceb64](https://github.com/pando85/kaniop/commit/4dceb64d01d9638c7ed94090048f5b3f44889efd))
- Ignore examples dir from yamllint ([63f4e85](https://github.com/pando85/kaniop/commit/63f4e85622f5a96d7fec7507e430c20bda08c579))
- Show container logs when e2e pod fails to start ([2ec3244](https://github.com/pando85/kaniop/commit/2ec3244333721544ca69475063ffeba090349daf))
