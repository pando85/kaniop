{{- define "kaniop.webhook.patch.serviceAccountName" -}}
{{- printf "%s-webhook-patch" (include "kaniop.fullname" .) }}
{{- end }}
