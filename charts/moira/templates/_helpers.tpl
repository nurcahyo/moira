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

{{/*
Template-time deployment guards.

These are a *first* line of defence and remain one after plan 10: they run on
helm install/upgrade only, so `kubectl scale` walks past every one of them. The
enforceable ceiling is cluster.admissionEnabled, which puts the check in
PostgreSQL where a scale command cannot avoid it.
*/}}
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
{{- if .Values.cluster.admissionEnabled -}}
{{- if gt (int .Values.replicaCount) (int .Values.cluster.maxReplicas) -}}
{{- fail (printf "replicaCount %d exceeds cluster.maxReplicas %d: the extra pods would be denied an admission lease and crash-loop" (int .Values.replicaCount) (int .Values.cluster.maxReplicas)) -}}
{{- end -}}
{{/* Two independent ceilings. The HPA scales to autoscaling.maxReplicas; the
     database admits cluster.maxReplicas. When they disagree the symptom is pods
     in CrashLoopBackOff at exactly the moment load is highest. */}}
{{- if and .Values.autoscaling.enabled (gt (int .Values.autoscaling.maxReplicas) (int .Values.cluster.maxReplicas)) -}}
{{- fail (printf "autoscaling.maxReplicas %d exceeds cluster.maxReplicas %d: the HPA would scale to pods the database refuses to admit" (int .Values.autoscaling.maxReplicas) (int .Values.cluster.maxReplicas)) -}}
{{- end -}}
{{/* A terminating pod must have time to release its lease. If the grace period
     is shorter, SIGKILL beats the release and every replacement waits out
     leaseExpirySeconds before it is admitted. */}}
{{- if le (int .Values.terminationGracePeriodSeconds) (int .Values.cluster.leaseExpirySeconds) -}}
{{- fail (printf "terminationGracePeriodSeconds %d must exceed cluster.leaseExpirySeconds %d so a terminating pod releases its lease instead of leaving it to expire" (int .Values.terminationGracePeriodSeconds) (int .Values.cluster.leaseExpirySeconds)) -}}
{{- end -}}
{{/* Moira rejects this at startup too (Settings::validate); failing here turns a
     crash-loop into a message the operator reads before anything is applied. */}}
{{- if le (int .Values.cluster.leaseExpirySeconds) (int .Values.cluster.leaseHeartbeatSeconds) -}}
{{- fail (printf "cluster.leaseExpirySeconds %d must exceed cluster.leaseHeartbeatSeconds %d, or a live replica's lease is reclaimable before it renews" (int .Values.cluster.leaseExpirySeconds) (int .Values.cluster.leaseHeartbeatSeconds)) -}}
{{- end -}}
{{/* Leader election with workers off elects a leader for zero jobs, and the
     retention sweep never runs on any replica. That deploys green and does
     nothing, which is the failure this refuses to ship. */}}
{{- if and (gt (int .Values.cluster.maxReplicas) 1) (not .Values.workers.enabled) -}}
{{- fail "cluster.maxReplicas > 1 requires workers.enabled: with workers off there is no singleton job for leader election to gate and no retention sweep runs on any replica" -}}
{{- end -}}
{{- end -}}
{{- end -}}
