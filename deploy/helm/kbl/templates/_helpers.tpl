{{- define "kbl.fullname" -}}
{{- if contains .Chart.Name .Release.Name -}}{{ .Release.Name | trunc 63 | trimSuffix "-" }}
{{- else -}}{{ printf "%s-%s" .Release.Name .Chart.Name | trunc 63 | trimSuffix "-" }}{{- end -}}
{{- end -}}

{{- define "kbl.labels" -}}
helm.sh/chart: {{ printf "%s-%s" .Chart.Name .Chart.Version }}
app.kubernetes.io/name: kbl
app.kubernetes.io/instance: {{ .Release.Name }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
app.kubernetes.io/version: {{ .Chart.AppVersion | quote }}
{{- end -}}

{{/* selector labels for a component: kbl.selector (dict "root" . "component" "tracker") */}}
{{- define "kbl.selector" -}}
app.kubernetes.io/name: kbl
app.kubernetes.io/instance: {{ .root.Release.Name }}
app.kubernetes.io/component: {{ .component }}
{{- end -}}

{{- define "kbl.image" -}}
{{ .root.Values.image.registry }}/{{ .name }}:{{ .root.Values.image.tag | default .root.Chart.AppVersion }}
{{- end -}}

{{- define "kbl.caConfigMap" -}}
{{- if .Values.felix.ca.existingConfigMap -}}{{ .Values.felix.ca.existingConfigMap }}{{- else -}}{{ include "kbl.fullname" . }}-felix-ca{{- end -}}
{{- end -}}

{{/* Container hardening shared by every container but the root init steps. */}}
{{- define "kbl.containerSecurity" -}}
allowPrivilegeEscalation: false
readOnlyRootFilesystem: true
runAsNonRoot: true
capabilities:
  drop: [ALL]
seccompProfile:
  type: RuntimeDefault
{{- end -}}

{{/* A one-shot root step that copies a Secret file to a memory volume the
     service user owns with mode 0400, which the token file and the GitHub App
     key must have. It keeps only CHOWN. args: root, image, src, dest, mount */}}
{{- define "kbl.copySecretInit" -}}
- name: {{ .name }}
  image: {{ .image }}
  imagePullPolicy: {{ .root.Values.image.pullPolicy }}
  command: [install, -m, "0400", -o, "10001", -g, "10001", {{ .src | quote }}, {{ .dest | quote }}]
  securityContext:
    allowPrivilegeEscalation: false
    readOnlyRootFilesystem: true
    runAsUser: 0
    runAsNonRoot: false
    capabilities:
      drop: [ALL]
      add: [CHOWN]
    seccompProfile:
      type: RuntimeDefault
  resources:
    requests: { cpu: 10m, memory: 8Mi }
    limits: { memory: 32Mi }
  volumeMounts:
    - { name: {{ .srcVolume }}, mountPath: {{ .srcDir }}, readOnly: true }
    - { name: run-kbl, mountPath: /run/kbl }
{{- end -}}
