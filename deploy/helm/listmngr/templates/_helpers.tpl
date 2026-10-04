{{- define "listmngr.name" -}}
{{- default .Chart.Name .Values.nameOverride | trunc 63 | trimSuffix "-" -}}
{{- end -}}

{{- define "listmngr.fullname" -}}
{{- if .Values.fullnameOverride -}}
{{- .Values.fullnameOverride | trunc 63 | trimSuffix "-" -}}
{{- else -}}
{{- printf "%s-%s" .Release.Name (include "listmngr.name" .) | trunc 63 | trimSuffix "-" -}}
{{- end -}}
{{- end -}}

{{- define "listmngr.labels" -}}
helm.sh/chart: {{ printf "%s-%s" .Chart.Name .Chart.Version | quote }}
app.kubernetes.io/name: {{ include "listmngr.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
app.kubernetes.io/version: {{ default .Chart.AppVersion .Values.image.tag | quote }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
{{- end -}}

{{- define "listmngr.selectorLabels" -}}
app.kubernetes.io/name: {{ include "listmngr.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
{{- end -}}

{{- define "listmngr.image" -}}
{{- if .Values.image.digest -}}
{{ .Values.image.repository }}@{{ .Values.image.digest }}
{{- else -}}
{{ .Values.image.repository }}:{{ default .Chart.AppVersion .Values.image.tag }}
{{- end -}}
{{- end -}}

{{- define "listmngr.secretName" -}}
{{- default (include "listmngr.fullname" .) .Values.existingSecret -}}
{{- end -}}

{{- define "listmngr.filesSecretName" -}}
{{- default (printf "%s-files" (include "listmngr.fullname" .)) .Values.existingFilesSecret -}}
{{- end -}}

{{- define "listmngr.hasSecretFiles" -}}
{{- if or .Values.secretFiles .Values.existingFilesSecret -}}true{{- end -}}
{{- end -}}
