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
app.kubernetes.io/component: listmngr
{{- end -}}

{{- define "listmngr.postgresqlSelectorLabels" -}}
app.kubernetes.io/name: {{ include "listmngr.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
app.kubernetes.io/component: postgresql
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

{{- define "listmngr.serviceAccountName" -}}
{{- if .Values.serviceAccount.create -}}
{{- default (include "listmngr.fullname" .) .Values.serviceAccount.name -}}
{{- else -}}
{{- default "default" .Values.serviceAccount.name -}}
{{- end -}}
{{- end -}}

{{- define "listmngr.postgresqlName" -}}
{{- printf "%s-postgresql" (include "listmngr.fullname" .) -}}
{{- end -}}

{{- define "listmngr.postgresqlSecretName" -}}
{{- default (include "listmngr.postgresqlName" .) .Values.postgresql.auth.existingSecret -}}
{{- end -}}

{{- define "listmngr.postgresqlImage" -}}
{{- with .Values.postgresql.image -}}
{{- if .digest -}}{{ .repository }}:{{ .tag }}@{{ .digest }}{{- else -}}{{ .repository }}:{{ .tag }}{{- end -}}
{{- end -}}
{{- end -}}

{{/* Where the database is: refuse an install that would have none. */}}
{{- define "listmngr.databaseCheck" -}}
{{- if .Values.postgresql.enabled -}}
{{- if hasKey .Values.secrets "LISTMNGR__DATABASE__URL" -}}
{{- fail "postgresql.enabled assembles LISTMNGR__DATABASE__URL; do not set secrets.LISTMNGR__DATABASE__URL as well" -}}
{{- end -}}
{{- if and (not .Values.postgresql.auth.password) (not .Values.postgresql.auth.existingSecret) -}}
{{- fail "postgresql.auth.password is required when postgresql.enabled (or postgresql.auth.existingSecret with a `password` key)" -}}
{{- end -}}
{{- else if and (not (hasKey .Values.secrets "LISTMNGR__DATABASE__URL")) (not .Values.existingSecret) -}}
{{- fail "no database: set postgresql.enabled, or secrets.LISTMNGR__DATABASE__URL (or an existingSecret carrying it)" -}}
{{- end -}}
{{- end -}}

{{/* The database environment of the migrate and serve containers. */}}
{{- define "listmngr.databaseEnv" -}}
{{- if .Values.postgresql.enabled }}
- name: POSTGRES_PASSWORD
  valueFrom:
    secretKeyRef:
      name: {{ include "listmngr.postgresqlSecretName" . }}
      key: password
- name: LISTMNGR__DATABASE__URL
  value: postgres://{{ .Values.postgresql.auth.username }}:$(POSTGRES_PASSWORD)@{{ include "listmngr.postgresqlName" . }}:5432/{{ .Values.postgresql.auth.database }}
{{- end }}
{{- end -}}
