---
name: examples
description: Guidelines for creating and maintaining example YAML files in the cmd/examples directory. Use when adding new CRD fields or modifying example code.
---

## Examples Generation Guidelines

This skill documents the conventions for creating and maintaining example YAML files in the `cmd/examples` directory. Following these guidelines ensures consistent, informative documentation for users.

### Core Principles

1. **Concrete Values**: Always set concrete values for fields in example code. Do NOT use comments to explain how to use a field.
   - Example code values are rendered in the generated YAML documentation
   - Comments in Rust code (`//`) are NOT rendered in the output YAML
   - Use real, representative values that users can understand

2. **Documentation in CRD Definitions**: All field documentation must be in the CRD struct definition, NOT in example code comments.
   - CRD doc comments (`///`) are extracted by the schema generator and rendered in YAML
   - Example code comments are discarded during YAML generation
   - This ensures documentation is consistent with the schema

3. **New Fields Must Be Demonstrated**: When adding new optional fields, update the example to show the field with a concrete value.
   - Users should see the field's shape immediately in the generated YAML
   - Set the field to `Some(...)` with a representative value, not `None`

### Workflow

#### When Adding a New Field to a CRD

1. **Add documentation to the CRD struct** (in `libs/*/src/crd.rs`):
   ```rust
   /// Template for referencing a pre-existing PersistentVolumeClaim.
   /// This allows users to manage PVCs externally.
   ///
   /// Template variables available:
   /// - `{kanidm_name}`: Kanidm CR name
   /// - `{statefulset_name}`: StatefulSet name
   ///
   /// Example: `{kanidm_name}-data` resolves to `my-idm-data`.
   #[serde(skip_serializing_if = "Option::is_none")]
   pub existing_claim_template: Option<String>,
   ```

2. **Set a concrete value in the example** (in `cmd/examples/src/*.rs`):
   ```rust
   // CORRECT: Concrete value that will be rendered in YAML
   existing_claim_template: Some("my-kanidm-data".to_string()),
   
   // WRONG: Commented explanation (NOT rendered in YAML)
   // existing_claim_template: Some("my-kanidm-data".to_string()),
   existing_claim_template: None,
   ```

3. **Regenerate examples**:
   ```bash
   make examples
   ```

#### When Modifying an Existing Field

1. **Update CRD documentation first** if the field's behavior changes
2. **Update the example value** to reflect the new behavior
3. **Regenerate examples**

### Example Code Structure

Example files (`cmd/examples/src/*.rs`) should:

- **Never contain explanatory comments about field usage** - that belongs in CRD definitions
- **Always use concrete, representative values** for all demonstrated fields
- **Follow the same structure as the CRD** for clarity
- **Use realistic naming** that matches the context (e.g., `my-idm`, `default`)

### YAML Generation Process

The YAML generator (`cmd/examples/src/yaml.rs`):

1. Serializes the example struct to YAML
2. Adds documentation comments from the CRD schema
3. Optional fields (not in `required` array) are commented out with `#`
4. Required fields are rendered without comments

**Key insight**: The YAML generator reads documentation from the CRD schema, not from example code comments. This is why all documentation must be in CRD definitions.

### Common Mistakes to Avoid

| Mistake | Why It's Wrong | Fix |
|---------|---------------|-----|
| Adding comments in example code | Not rendered in YAML output | Move to CRD doc comments |
| Setting new fields to `None` | Field doesn't appear in generated YAML | Set `Some(...)` with concrete value |
| Hand-editing generated YAML files | Will be overwritten by `make examples` | Only modify example code or CRD |
| Documenting behavior in examples | Documentation won be consistent | Document in CRD definition |

### Verification

After modifying examples or CRD definitions:

1. Run `make examples` to regenerate YAML files
2. Check the generated YAML to verify:
   - New fields appear with proper documentation
   - Documentation comments come from CRD definitions
   - Concrete values are rendered correctly

### Related Commands

```bash
# Regenerate example YAML files
make examples

# Regenerate CRD YAML after CRD changes
make crdgen

# Run both after CRD modifications
make crdgen && make examples
```

### Related Files

- `cmd/examples/src/*.rs` - Example struct definitions
- `cmd/examples/src/yaml.rs` - YAML generation logic
- `libs/*/src/crd.rs` - CRD definitions with documentation
- `examples/*.yaml` - Generated example YAML files
- `charts/kaniop/crds/crds.yaml` - Generated CRD YAML