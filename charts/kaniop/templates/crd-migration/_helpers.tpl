{{- define "kaniop.crdMigration.serviceAccountName" -}}
{{- printf "%s-crd-migrator" (include "kaniop.fullname" .) }}
{{- end }}

{{- define "kaniop.crdMigration.selectorLabels" -}}
{{- include "kaniop.commonSelectorLabels" . }}
app.kubernetes.io/component: crd-migration
{{- end }}

{{- define "kaniop.crdMigration.labels" -}}
{{- include "kaniop.commonLabels" . }}
{{ include "kaniop.crdMigration.selectorLabels" . }}
{{- end }}
