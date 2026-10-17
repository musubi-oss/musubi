{{- define "musubi.name" -}}
{{- default .Chart.Name .Values.nameOverride | trunc 63 | trimSuffix "-" -}}
{{- end -}}

{{- define "musubi.fullname" -}}
{{- printf "%s-%s" .Release.Name (include "musubi.name" .) | trunc 63 | trimSuffix "-" -}}
{{- end -}}

{{- define "musubi.labels" -}}
app.kubernetes.io/name: {{ include "musubi.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
app.kubernetes.io/version: {{ .Chart.AppVersion | quote }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
{{- end -}}

{{- define "musubi.selectorLabels" -}}
app.kubernetes.io/name: {{ include "musubi.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
{{- end -}}

{{- define "musubi.serviceAccountName" -}}
{{- if .Values.serviceAccount.create -}}
{{- default (include "musubi.fullname" .) .Values.serviceAccount.name -}}
{{- else -}}
{{- default "default" .Values.serviceAccount.name -}}
{{- end -}}
{{- end -}}
