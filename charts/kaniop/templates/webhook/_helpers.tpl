{{- define "kaniop.webhook.version" -}}
{{ .Values.webhook.image.tag | default .Chart.AppVersion }}
{{- end }}

{{- define "kaniop.webhook.selectorLabels" -}}
{{- include "kaniop.commonSelectorLabels" . }}
app.kubernetes.io/component: webhook
{{- end }}

{{- define "kaniop.webhook.labels" -}}
{{- include "kaniop.commonLabels" . }}
{{ include "kaniop.webhook.selectorLabels" . }}
{{- end }}
