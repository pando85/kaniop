# DNS resolution

[Issue description](https://github.com/kanidm/kanidm/issues/3188).

## The state of the art

When a member IP changes in a distributed system like Kafka, Elasticsearch, or Patroni, the system must handle the change to maintain cluster stability and communication. Here's what typically happens:

### Kafka

1. **DNS Resolution**: Kafka relies on DNS names for broker discovery. If a broker's IP changes but its DNS name remains the same, the DNS resolution will handle the change.
2. **ZooKeeper Update**: The broker will re-register itself with ZooKeeper using the new IP address.
3. **Client Reconnection**: Clients using the broker's DNS name will resolve to the new IP address and reconnect.
4. **Broker Communication**: Other brokers will use the DNS name to resolve the new IP address and continue communication.

### Elasticsearch

1. **DNS Resolution**: Elasticsearch nodes use DNS names for discovery. If a node's IP changes, the DNS resolution will handle the change.
2. **Cluster State Update**: The node will update its IP address in the cluster state, which is managed by the master node.
3. **Node Reconnection**: Other nodes will use the DNS name to resolve the new IP address and reconnect.
4. **Replica Reallocation**: Elasticsearch may reallocate replicas if necessary to maintain high availability.

### Patroni

1. **DNS Resolution**: Patroni nodes use DNS names for cluster member identification. If a node's IP changes, the DNS resolution will handle the change.
2. **Consensus System Update**: The node will update its IP address in the distributed consensus system (Etcd, Consul, or ZooKeeper).
3. **Cluster Reconfiguration**: Patroni will reconfigure the cluster using the new IP address.
4. **Failover Handling**: If the IP change causes a node to become unreachable temporarily, Patroni may trigger a failover to maintain availability.

### General Steps

1. **DNS Update**: Ensure the DNS records are updated to reflect the new IP address.
2. **Service Restart**: Restart the service on the node with the new IP address to re-register with the cluster.
3. **Health Checks**: The system will perform health checks to verify the node's availability.
4. **Reconnection**: Other nodes and clients will reconnect using the updated DNS resolution.

MongoDB uses DNS names to manage communication between nodes in a replica set. When a node's IP address changes, MongoDB relies on DNS resolution to update the IP addresses dynamically. Here’s how MongoDB handles this process:

### MongoDB

1. **DNS Configuration**: Nodes in a MongoDB replica set are typically configured using DNS names rather than static IP addresses. This allows MongoDB to resolve the current IP address of each node dynamically.

2. **DNS Resolution**: When a node's IP address changes, the DNS record for that node is updated to reflect the new IP address. MongoDB nodes use DNS resolution to obtain the current IP address associated with each node's hostname.

3. **Heartbeat Mechanism**: MongoDB nodes send heartbeats to each other to monitor the health and status of the replica set members. These heartbeats use the DNS names to resolve the current IP addresses of the nodes.

4. **Automatic Update**: When a node's IP address changes, the other nodes in the replica set will automatically resolve the new IP address through DNS during their regular heartbeat checks. This allows the nodes to continue communicating without manual intervention.

5. **Replica Set Configuration**: The replica set configuration (`rs.conf()`) contains the hostnames of the members. As long as the hostnames remain the same, MongoDB will use DNS to resolve the new IP addresses.

### 389 Directory Server

1. **DNS Resolution**:

   - **Hostname Configuration**: 389-ds is configured to use hostnames rather than static IP addresses for replication agreements and communication between nodes.
   - **Dynamic Resolution**: When a node needs to communicate with another node, it performs a DNS lookup to resolve the hostname to the current IP address. This lookup is handled by the underlying operating system's DNS resolver.

2. **Replication Agreements**:

   - **Stable Hostnames**: Replication agreements in 389-ds use hostnames to identify the replication partners. These hostnames are resolved to IP addresses dynamically.
   - **Automatic Updates**: When a node’s IP address changes, the DNS record for the hostname is updated. The next time the replication agreement is used, the DNS resolver will obtain the new IP address.

3. **Heartbeat and Monitoring**:

   - **Health Checks**: 389-ds performs regular health checks and heartbeats to monitor the status of replication partners. These checks use the configured hostnames.
   - **Failure Detection**: If a node becomes unreachable, 389-ds will detect the failure through missed heartbeats and attempt to reconnect using the hostname, which will resolve to the new IP address if it has changed.

4. **Kubernetes Integration**:
   - **StatefulSet Hostnames**: In a Kubernetes environment, StatefulSets provide stable network identities for pods. Each pod gets a unique, stable hostname that Kubernetes manages.
   - **DNS Management**: Kubernetes automatically updates DNS records when a pod’s IP address changes. This ensures that the hostname always resolves to the current IP address of the pod.

### Example: Kafka Broker Configuration

```properties
# Broker configuration with DNS name
advertised.listeners=PLAINTEXT://kafka-broker1.example.com:9092
```

### Example: Elasticsearch Node Configuration

```yaml
discovery.seed_hosts:
  - es-node1.example.com
  - es-node2.example.com
  - es-node3.example.com
```

### Example: Patroni Node Configuration

```yaml
restapi:
  connect_address: postgresql0.example.com:8008
```

By using DNS names, these systems can handle IP changes more gracefully, relying on DNS resolution to manage the new IP addresses.

## Proposed solution

Heartbeat mechanism could be a valid solution to update IP resolution.
