# libgendex Helm chart

Requires Kubernetes 1.26+ and Helm with OCI support (3.8+; CI uses 4.3.0).

Install from GitHub Container Registry:

```sh
helm upgrade --install libgendex oci://ghcr.io/wexder/charts/libgendex \
  --version 0.2.0 --namespace libgendex --create-namespace
```

For a checkout before publication, replace the OCI reference with `./charts/libgendex` and set
`--set image.repository=YOUR_IMAGE --set image.tag=YOUR_TAG`.

## Storage and updates

One replica owns the Tantivy index, SQLite staging files, and FTP cache. The chart rejects more
than one replica and uses a Deployment with `Recreate` updates. An upgrade causes a short outage.
Restarting during bootstrap starts that import again; retained source ranges remain reusable.

| Value | Default | Meaning |
| --- | --- | --- |
| `image.repository` | `ghcr.io/wexder/libgendex` | Container repository |
| `image.tag` | chart appVersion | Release tag without the leading `v` |
| `image.digest` | empty | Optional immutable digest; overrides the tag |
| `imagePullSecrets` | `[]` | Existing Kubernetes registry secrets |
| `persistence.data.size` | `50Gi` | Index, state, bounded FTP cache, and staging |
| `persistence.library.size` | `100Gi` | Downloaded books |
| `persistence.library.nfs.enabled` | `false` | Mount an NFS export directly at `/library` |
| `persistence.library.nfs.server` / `.path` | empty | NFS server and absolute export path |
| `persistence.library.nfs.readOnly` | `false` | Make the library mount read-only |
| `persistence.*.storageClass` | empty | Cluster default storage class |
| `persistence.*.existingClaim` | empty | Use an existing PVC instead of creating one |
| `persistence.*.retain` | `true` | Preserve chart-created PVCs on uninstall |
| `persistence.*.enabled` | `true` | If false and no existingClaim or library NFS, use ephemeral emptyDir |
| `config.toml` | empty | Non-secret TOML settings mounted at `/app/bookjev.toml` |
| `config.existingSecret` | empty | Existing Secret containing the `bookjev.toml` key |
| `extraEnv` / `extraEnvFrom` | `[]` | Additional variables and references to ConfigMaps/Secrets |
| `ingress.enabled` | `false` | Create an Ingress |
| `resources.requests` | 1 CPU, 1 GiB | Reserved compute resources |
| `resources.limits` | 4 CPUs, 4 GiB | Compute limits |

The PVC sizes are starting values, not estimates of every snapshot. Allow capacity for temporary
staging and the final index in addition to the 512 MiB cache. SQLite temporary files use writable
`/tmp` on node ephemeral storage. Monitor both PVC and node disk space during bootstrap. The image
runs as UID/GID 1000 with a read-only root filesystem; fsGroup 1000 grants access to mounted storage.
Existing storage must support those ownership/permission settings.

Back up `/data` and `/library` before upgrades. Chart-created PVCs remain after uninstall when
`retain=true`; reattach them using `existingClaim` when reinstalling under a new release name.
Setting `retain=false` before uninstall removes the claims; the StorageClass reclaim policy then
determines what happens to the underlying volumes. PVC expansion requires StorageClass support.

## Library on NFS

To share the ebooks export with Kavita, add this to your values file:

```yaml
persistence:
  library:
    nfs:
      enabled: true
      server: 192.168.240.112
      path: /mnt/main_pool/private/media/ebooks
      readOnly: false
```

This mounts the existing export directly at `/library` and skips creation of the library PVC.
The index, FTP cache, and staging continue to use the data PVC. NFS takes precedence over
`persistence.library.enabled`; leave `existingClaim` empty when enabling NFS.
The export must allow writes by the application's UID/GID 1000 for server-side book downloads.
With `readOnly: true`, server-side library downloads cannot save books.
The NFS export must already exist and be reachable from the Kubernetes nodes; see
[Kubernetes NFS volumes](https://kubernetes.io/docs/concepts/storage/volumes/#nfs).
The chart does not create, delete, or manage the contents of the export.

## Ingress and configuration

Example `values-production.yaml`:

```yaml
ingress:
  enabled: true
  className: nginx
  hosts:
    - host: books.example.com
      paths:
        - path: /
          pathType: Prefix
  tls:
    - secretName: books-tls
      hosts: [books.example.com]

config:
  toml: |
    [indexer]
    ftp_cache_mb = 512
    refresh_interval = "24h"

extraEnv:
  - name: BOOKJEV_RANKING__PROVIDER
    value: api
  - name: BOOKJEV_RANKING__API__URL
    value: https://scorer.example.com
  - name: BOOKJEV_RANKING__API__API_KEY
    valueFrom:
      secretKeyRef:
        name: scorer-credentials
        key: api-key
```

The ingress controller and TLS Secret must already exist. Apply with `helm upgrade --install ...
-f values-production.yaml`. Application settings follow the [root configuration documentation](../../README.md#configuration).
Choose either `config.toml` or `config.existingSecret`. ConfigMap changes roll the pod through a
checksum annotation; changes to external Secrets require a rollout restart. Pod paths and listen
address are configured through environment variables; retain `/data`, `/library`, and port 8080
when supplying application TOML. Published images support ranking providers `none` and `api`;
provider `local` needs a custom image built with `FEATURES=local`.

## Private GHCR packages

Authenticate Helm with a GitHub token that can read the chart package:

```sh
printf '%s' "$GHCR_TOKEN" | helm registry login ghcr.io --username YOUR_GITHUB_USER --password-stdin
```

Create a Kubernetes image-pull Secret using your cluster's secret-management process, then set:

```yaml
imagePullSecrets:
  - name: ghcr
```

Helm registry credentials download the chart; Kubernetes image-pull credentials download the
container. They are separate. Public packages can be pulled without these credentials.

## Check the chart

```sh
helm lint charts/libgendex --strict
helm template libgendex charts/libgendex
```

CI renders default storage, existing claims, ephemeral storage, ingress, and config variants.
See [Helm OCI registries](https://helm.sh/docs/topics/registries/) for registry command syntax.
