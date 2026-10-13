{{/*
A backend's data claim. Skipped entirely when the operator supplied an
existing claim, which the chart then never owns, or chose ephemeral storage. New shared claims carry
Helm's keep policy so an uninstall cannot discard collected data.
Call with a dict: "name" and "storage".
*/}}
{{- define "common.pvc" -}}
{{- $storage := .storage -}}
{{- if not (or $storage.existingClaim $storage.ephemeral) }}
apiVersion: v1
kind: PersistentVolumeClaim
metadata:
  name: {{ .name }}
  {{- if $storage.retain }}
  annotations: {helm.sh/resource-policy: keep}
  {{- end }}
spec:
  {{- if ne $storage.storageClassName nil }}
  storageClassName: {{ $storage.storageClassName | quote }}
  {{- end }}
  accessModes: [ReadWriteOnce]
  resources:
    requests:
      storage: {{ $storage.size | quote }}
{{- end }}
{{- end -}}

{{/* The claim a pod mounts: the operator's, or the one this chart created. */}}
{{- define "common.claimName" -}}
{{- default .name .storage.existingClaim -}}
{{- end -}}


{{/*
The data volume source a backend pod mounts: its claim, or an emptyDir when
storage.ephemeral is set. An emptyDir loses all data whenever the pod leaves
its node; it exists for clusters without any usable StorageClass.
Call with a dict: "name" and "storage".
*/}}
{{- define "common.dataVolumeSource" -}}
{{- if .storage.ephemeral -}}
emptyDir: {sizeLimit: {{ .storage.size | quote }}}
{{- else -}}
persistentVolumeClaim: {claimName: {{ include "common.claimName" . | quote }}}
{{- end -}}
{{- end -}}
