{{/*
Expand the name of the chart.
*/}}
{{- define "kaniop.name" -}}
{{- default .Chart.Name .Values.nameOverride | trunc 63 | trimSuffix "-" }}
{{- end }}

{{/*
Create a default fully qualified app name.
We truncate at 63 chars because some Kubernetes name fields are limited to this (by the DNS naming spec).
If release name contains chart name it will be used as a full name.
*/}}
{{- define "kaniop.fullname" -}}
{{- if .Values.fullnameOverride -}}
{{- .Values.fullnameOverride | trunc 63 | trimSuffix "-" -}}
{{- else -}}
{{- $name := default .Chart.Name .Values.nameOverride -}}
{{- if contains $name .Release.Name -}}
{{- .Release.Name | trunc 63 | trimSuffix "-" -}}
{{- else -}}
{{- printf "%s-%s" .Release.Name $name | trunc 63 | trimSuffix "-" -}}
{{- end }}
{{- end }}
{{- end }}

{{- define "kaniop.version" -}}
{{ .Values.image.tag | default .Chart.AppVersion }}
{{- end }}

{{- define "kaniop.labels" -}}
{{- include "kaniop.selectorLabels" . }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
app.kubernetes.io/part-of: kaniop
app.kubernetes.io/version: {{ include "kaniop.version" . | splitList "@" | first | trunc 63 | quote }}
{{- with .Values.additionalLabels }}
{{ toYaml . }}
{{- end }}
{{- end }}

{{- define "kaniop.selectorLabels" -}}
app.kubernetes.io/name: {{ include "kaniop.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
{{- end }}

{{/*
Create the name of the service account to use
*/}}
{{- define "kaniop.serviceAccountName" -}}
{{- default (include "kaniop.name" .) .Values.serviceAccount.name }}
{{- end }}
