{{- define "moira.name" -}}
{{- default .Chart.Name .Values.nameOverride | trunc 63 | trimSuffix "-" -}}
{{- end -}}

{{- define "moira.fullname" -}}
{{- default (include "moira.name" .) .Values.fullnameOverride | trunc 63 | trimSuffix "-" -}}
{{- end -}}

{{- define "moira.labels" -}}
app.kubernetes.io/name: {{ include "moira.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
app.kubernetes.io/version: {{ .Chart.AppVersion | quote }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
{{- end -}}

{{- define "moira.validateDeployment" -}}
{{- if ne (int .Values.replicaCount) 1 -}}
{{- fail "Moira MVP requires replicaCount=1 because concurrency and rate limits are process-local" -}}
{{- end -}}
{{- if .Values.autoscaling.enabled -}}
{{- fail "Moira MVP does not support autoscaling because concurrency and rate limits are process-local" -}}
{{- end -}}
{{- if not .Values.secret.name -}}
{{- fail "secret.name must reference an existing Secret" -}}
{{- end -}}
{{- end -}}
