{{/*
Helpers for the Moira console chart.

Shape mirrors charts/moira/templates/_helpers.tpl. The validation block is
larger because the console has more ways to be misconfigured into something
that renders cleanly and then fails at deploy time or, worse, runs insecurely.

Deliberate omission relative to charts/moira/templates/: there is no
servicemonitor.yaml here. See the header of values.yaml for the reason and for
the condition that reverses it.
*/}}

{{- define "moira-console.name" -}}
{{- default .Chart.Name .Values.nameOverride | trunc 63 | trimSuffix "-" -}}
{{- end -}}

{{- define "moira-console.fullname" -}}
{{- default (include "moira-console.name" .) .Values.fullnameOverride | trunc 63 | trimSuffix "-" -}}
{{- end -}}

{{- define "moira-console.labels" -}}
app.kubernetes.io/name: {{ include "moira-console.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
app.kubernetes.io/version: {{ .Chart.AppVersion | quote }}
app.kubernetes.io/component: console
app.kubernetes.io/part-of: moira
app.kubernetes.io/managed-by: {{ .Release.Service }}
{{- end -}}

{{- define "moira-console.selectorLabels" -}}
app.kubernetes.io/name: {{ include "moira-console.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
{{- end -}}

{{/*
Image reference for the migration job: falls back to the application image when
migrationJob.image.repository is unset.
*/}}
{{- define "moira-console.migrationImage" -}}
{{- $repository := default .Values.image.repository .Values.migrationJob.image.repository -}}
{{- $tag := default .Values.image.tag .Values.migrationJob.image.tag -}}
{{- printf "%s:%s" $repository $tag -}}
{{- end -}}

{{/*
Fail fast on configurations that render but do not work.

Every check here exists because the failure it prevents is invisible in CI:
`helm template` and `kubeconform` both pass on a manifest that puts a signing
key in a ConfigMap or points a PodDisruptionBudget at more replicas than exist.
*/}}
{{- define "moira-console.validate" -}}

{{/* --- credentials must come from a Secret, never from the ConfigMap ------ */}}
{{- $deniedKeys := list "BETTER_AUTH_SECRET" "CONSOLE_SECRET_ENCRYPTION_KEY" "CONSOLE_DATABASE_URL" "MOIRA_SYSTEM_KEY" -}}
{{- range $key, $value := .Values.config -}}
  {{- if has $key $deniedKeys -}}
    {{- fail (printf "config.%s is credential material: put it in the Secret named by secret.name, never in the ConfigMap (a ConfigMap is world-readable to anything with get on the namespace)" $key) -}}
  {{- end -}}
  {{- if regexMatch "(SECRET|PASSWORD|CREDENTIAL|PRIVATE|_TOKEN$|_KEY$|DATABASE_URL)" $key -}}
    {{- fail (printf "config.%s looks like credential material and must not be rendered into a ConfigMap; move it to the Secret named by secret.name" $key) -}}
  {{- end -}}
  {{/* PORT and HOSTNAME are set from .Values.containerPort by the Deployment's
       own `env:` block, which overrides `envFrom`. Accepting them here would
       let someone set a value that is silently ignored. */}}
  {{- if or (eq $key "PORT") (eq $key "HOSTNAME") -}}
    {{- fail (printf "config.%s is derived from containerPort by templates/deployment.yaml and would be silently overridden; set containerPort instead" $key) -}}
  {{- end -}}
{{- end -}}

{{/* --- the Secret this chart injects but does not create ------------------ */}}
{{- if not .Values.secret.name -}}
{{- fail "secret.name must reference a Secret carrying BETTER_AUTH_SECRET, CONSOLE_SECRET_ENCRYPTION_KEY, CONSOLE_DATABASE_URL and MOIRA_SYSTEM_KEY" -}}
{{- end -}}
{{- if and .Values.secret.create (not .Values.secret.data) -}}
{{- fail "secret.create=true with an empty secret.data would create an empty Secret and every pod would boot without credentials" -}}
{{- end -}}

{{/* --- origins ------------------------------------------------------------
     Names are the ones console/lib/env.ts actually reads. Anything missing here
     is a ConsoleConfigError thrown at boot, i.e. a crash-looping pod; anything
     misspelled is the same failure wearing a different hat. Catching it during
     `helm template` is strictly cheaper.                                     */}}
{{/* Every lookup below goes through `toString`. `--set config.X=true` and
     `--set config.X=3000` hand Helm a bool and an int, and `hasPrefix`/`eq`
     against those raise "incompatible types for comparison" — a template crash
     with no message about what the operator actually did wrong. */}}
{{- $consoleOrigin := toString (default "" (get .Values.config "CONSOLE_PUBLIC_ORIGIN")) -}}
{{- if not $consoleOrigin -}}
{{- fail "config.CONSOLE_PUBLIC_ORIGIN is required: Better Auth uses it as baseURL and as its only trustedOrigins entry, which is the CSRF check" -}}
{{- end -}}
{{- if and (not (hasPrefix "https://" $consoleOrigin)) (not .Values.allowInsecureConsoleOrigin) -}}
{{- fail (printf "config.CONSOLE_PUBLIC_ORIGIN must be https:// for an internet-facing console (got %q); set allowInsecureConsoleOrigin=true only for a local cluster" $consoleOrigin) -}}
{{- end -}}
{{- if not (get .Values.config "MOIRA_API_URL") -}}
{{- fail "config.MOIRA_API_URL is required: it is how the console reaches Moira's admin API" -}}
{{- end -}}
{{- if not (get .Values.config "MOIRA_ADMIN_API_AUDIENCE") -}}
{{- fail "config.MOIRA_ADMIN_API_AUDIENCE must be non-empty: Moira skips audience validation entirely when a trusted issuer's expected_audiences is empty, so an empty value turns a check off instead of failing loudly" -}}
{{- end -}}
{{/* MOIRA_BFF_ISSUER_URL is optional in env.ts (defaults to the console origin),
     so it is not required here — but if it is set it must still be https. */}}
{{- $issuerUrl := toString (default "" (get .Values.config "MOIRA_BFF_ISSUER_URL")) -}}
{{- if and $issuerUrl (not (hasPrefix "https://" $issuerUrl)) (not .Values.allowInsecureConsoleOrigin) -}}
{{- fail (printf "config.MOIRA_BFF_ISSUER_URL must be https:// (got %q): Moira's jwt-issuer SSRF check rejects anything else" $issuerUrl) -}}
{{- end -}}
{{/* Mirrors readConsoleEnv()'s own refusal, one deploy earlier. */}}
{{- $allowInsecure := toString (default "" (get .Values.config "CONSOLE_ALLOW_INSECURE_URLS")) -}}
{{- $nodeEnv := toString (default "" (get .Values.config "NODE_ENV")) -}}
{{- if and (or (eq $allowInsecure "true") (eq $allowInsecure "1")) (eq $nodeEnv "production") -}}
{{- fail "config.CONSOLE_ALLOW_INSECURE_URLS is a fixture-only knob and console/lib/env.ts refuses it under NODE_ENV=production; the pod would fail to boot" -}}
{{- end -}}

{{/* --- replicas, autoscaling, disruption budget --------------------------- */}}
{{- if .Values.autoscaling.enabled -}}
  {{- if lt (int .Values.autoscaling.minReplicas) 1 -}}
  {{- fail "autoscaling.minReplicas must be at least 1" -}}
  {{- end -}}
  {{- if lt (int .Values.autoscaling.maxReplicas) (int .Values.autoscaling.minReplicas) -}}
  {{- fail "autoscaling.maxReplicas must be greater than or equal to autoscaling.minReplicas" -}}
  {{- end -}}
{{- else -}}
  {{- if lt (int .Values.replicaCount) 1 -}}
  {{- fail "replicaCount must be at least 1" -}}
  {{- end -}}
{{- end -}}
{{- if .Values.podDisruptionBudget.enabled -}}
  {{- $floor := ternary (int .Values.autoscaling.minReplicas) (int .Values.replicaCount) .Values.autoscaling.enabled -}}
  {{- if ge (int .Values.podDisruptionBudget.minAvailable) $floor -}}
  {{- fail (printf "podDisruptionBudget.minAvailable (%d) must be less than the minimum replica count (%d), otherwise the budget can never be satisfied and node drains block forever; for a single-replica install set podDisruptionBudget.enabled=false" (int .Values.podDisruptionBudget.minAvailable) $floor) -}}
  {{- end -}}
{{- end -}}

{{/* --- ingress ------------------------------------------------------------ */}}
{{- if .Values.ingress.enabled -}}
  {{- if not .Values.ingress.host -}}
  {{- fail "ingress.host is required when ingress.enabled=true" -}}
  {{- end -}}
  {{- if and .Values.ingress.tls.enabled (not .Values.ingress.tls.secretName) -}}
  {{- fail "ingress.tls.secretName is required when ingress.tls.enabled=true" -}}
  {{- end -}}
{{- end -}}

{{/* --- migration job ------------------------------------------------------ */}}
{{- if .Values.migrationJob.enabled -}}
  {{- if not .Values.migrationJob.command -}}
  {{- fail "migrationJob.enabled=true requires an explicit migrationJob.command: the runtime image is distroless and contains no shell and no @better-auth/cli, so there is nothing sensible to default to" -}}
  {{- end -}}
{{- end -}}

{{- end -}}
