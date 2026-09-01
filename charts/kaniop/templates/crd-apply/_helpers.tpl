{{- define "kaniop.crdApply.serviceAccountName" -}}
{{- printf "%s-crd-applier" (include "kaniop.fullname" .) }}
{{- end }}

{{- define "kaniop.crdApply.selectorLabels" -}}
{{- include "kaniop.commonSelectorLabels" . }}
app.kubernetes.io/component: crd-apply
{{- end }}

{{- define "kaniop.crdApply.labels" -}}
{{- include "kaniop.commonLabels" . }}
{{ include "kaniop.crdApply.selectorLabels" . }}
{{- end }}
