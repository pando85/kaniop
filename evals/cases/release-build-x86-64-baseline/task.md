# Task

After upgrading a homelab cluster from kaniop 0.21.1 to 0.23.0, the operator pod
and the `kaniop-crd-apply` chart hook crash immediately with exit code 132 on one
node (an Intel Atom C2000). The same images run on the other nodes, and 0.21.1
runs on every node. Backup jobs on that node fail too: `kaniop-data-mover
--version` works, but the container dies with exit code 132 as soon as it talks
to the S3 endpoint.

No Rust code that could explain a crash before `main` changed between the two
releases, and CI was green for both. Find out why the 0.23.0 release images crash
on this CPU, fix the release build so that it cannot happen again silently, and
make CI fail if a shipped x86_64 binary would crash this way.
