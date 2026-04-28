#!/usr/bin/env bash
# Licensed to the Apache Software Foundation (ASF) under one
# or more contributor license agreements.  See the NOTICE file
# distributed with this work for additional information
# regarding copyright ownership.  The ASF licenses this file
# to you under the Apache License, Version 2.0 (the
# "License"); you may not use this file except in compliance
# with the License.  You may obtain a copy of the License at
#
#   http://www.apache.org/licenses/LICENSE-2.0
#
# Unless required by applicable law or agreed to in writing,
# software distributed under the License is distributed on an
# "AS IS" BASIS, WITHOUT WARRANTIES OR CONDITIONS OF ANY
# KIND, either express or implied.  See the License for the
# specific language governing permissions and limitations
# under the License.

set -euo pipefail

# Iggy Postgres-to-Iceberg Benchmark: Infrastructure Setup
# Matches the article's test environment:
#   - AWS RDS Aurora Postgres 16 Serverless, 48 ACUs max
#   - AWS S3 for Iceberg storage
#   - REST catalog (Nessie) on EKS
#   - EKS 1.34, m8i.xlarge nodes (4 CPU, 16 GB RAM)

REGION="${AWS_REGION:-us-east-1}"
CLUSTER_NAME="${EKS_CLUSTER_NAME:-iggy-benchmark}"
RDS_INSTANCE="${RDS_INSTANCE_NAME:-iggy-benchmark-pg}"
S3_BUCKET="${S3_BUCKET_NAME:-iggy-benchmark-iceberg}"
NESSIE_NAMESPACE="nessie"

echo "=== Iggy PG-to-Iceberg Benchmark Infrastructure ==="
echo "Region: $REGION"
echo "EKS Cluster: $CLUSTER_NAME"
echo "RDS Instance: $RDS_INSTANCE"
echo "S3 Bucket: $S3_BUCKET"

# --- S3 Bucket ---
echo ""
echo "--- Creating S3 bucket for Iceberg ---"
aws s3 mb "s3://$S3_BUCKET" --region "$REGION" 2>/dev/null || echo "Bucket already exists"

# --- RDS Aurora Postgres 16 Serverless ---
echo ""
echo "--- Creating RDS Aurora Postgres 16 Serverless ---"
echo "Note: Logical replication requires custom parameter group"

PG_PARAM_GROUP="iggy-bench-pg16-params"
aws rds create-db-cluster-parameter-group \
  --db-cluster-parameter-group-name "$PG_PARAM_GROUP" \
  --db-parameter-group-family aurora-postgresql16 \
  --description "Iggy benchmark - logical replication enabled" \
  --region "$REGION" 2>/dev/null || echo "Parameter group already exists"

aws rds modify-db-cluster-parameter-group \
  --db-cluster-parameter-group-name "$PG_PARAM_GROUP" \
  --parameters "ParameterName=rds.logical_replication,ParameterValue=1,ApplyMethod=pending-reboot" \
  --region "$REGION"

DB_PASSWORD=$(openssl rand -base64 24 | tr -d '/+=' | head -c 24)
echo "DB Password (save this): $DB_PASSWORD"

aws rds create-db-cluster \
  --db-cluster-identifier "$RDS_INSTANCE" \
  --engine aurora-postgresql \
  --engine-version "16.4" \
  --serverless-v2-scaling-configuration MinCapacity=2,MaxCapacity=48 \
  --master-username iggy_bench \
  --master-user-password "$DB_PASSWORD" \
  --db-cluster-parameter-group-name "$PG_PARAM_GROUP" \
  --database-name tpch \
  --vpc-security-group-ids "${VPC_SECURITY_GROUP}" \
  --db-subnet-group-name "${DB_SUBNET_GROUP}" \
  --region "$REGION" 2>/dev/null || echo "Cluster already exists"

aws rds create-db-instance \
  --db-instance-identifier "${RDS_INSTANCE}-1" \
  --db-instance-class db.serverless \
  --db-cluster-identifier "$RDS_INSTANCE" \
  --engine aurora-postgresql \
  --region "$REGION" 2>/dev/null || echo "Instance already exists"

echo "Waiting for RDS cluster to become available..."
aws rds wait db-cluster-available --db-cluster-identifier "$RDS_INSTANCE" --region "$REGION"

RDS_ENDPOINT=$(aws rds describe-db-clusters \
  --db-cluster-identifier "$RDS_INSTANCE" \
  --query "DBClusters[0].Endpoint" \
  --output text \
  --region "$REGION")
echo "RDS Endpoint: $RDS_ENDPOINT"

# --- EKS Cluster ---
echo ""
echo "--- Creating EKS cluster ---"
echo "Using eksctl for managed node group with m8i.xlarge instances"

cat > /tmp/iggy-bench-eks.yaml <<EOF
apiVersion: eksctl.io/v1alpha5
kind: ClusterConfig
metadata:
  name: $CLUSTER_NAME
  region: $REGION
  version: "1.34"
managedNodeGroups:
  - name: benchmark-nodes
    instanceType: m8i.xlarge
    desiredCapacity: 2
    minSize: 1
    maxSize: 4
    volumeSize: 100
    labels:
      workload: benchmark
EOF

eksctl create cluster -f /tmp/iggy-bench-eks.yaml 2>/dev/null || echo "Cluster already exists"
aws eks update-kubeconfig --name "$CLUSTER_NAME" --region "$REGION"

# --- Nessie (REST Catalog for Iceberg) ---
echo ""
echo "--- Deploying Nessie REST catalog ---"
kubectl create namespace "$NESSIE_NAMESPACE" 2>/dev/null || true

cat <<EOF | kubectl apply -n "$NESSIE_NAMESPACE" -f -
apiVersion: apps/v1
kind: Deployment
metadata:
  name: nessie
spec:
  replicas: 1
  selector:
    matchLabels:
      app: nessie
  template:
    metadata:
      labels:
        app: nessie
    spec:
      containers:
      - name: nessie
        image: ghcr.io/projectnessie/nessie:0.100.0
        ports:
        - containerPort: 19120
        env:
        - name: NESSIE_VERSION_STORE_TYPE
          value: IN_MEMORY
        resources:
          requests:
            cpu: "500m"
            memory: "1Gi"
          limits:
            cpu: "1"
            memory: "2Gi"
---
apiVersion: v1
kind: Service
metadata:
  name: nessie
spec:
  selector:
    app: nessie
  ports:
  - port: 19120
    targetPort: 19120
  type: ClusterIP
EOF

echo ""
echo "--- Deploying Iggy server ---"
cat <<EOF | kubectl apply -f -
apiVersion: apps/v1
kind: Deployment
metadata:
  name: iggy-server
  namespace: default
spec:
  replicas: 1
  selector:
    matchLabels:
      app: iggy-server
  template:
    metadata:
      labels:
        app: iggy-server
    spec:
      containers:
      - name: iggy
        image: apache/iggy:latest
        ports:
        - containerPort: 8090
          name: tcp
        - containerPort: 3000
          name: http
        resources:
          requests:
            cpu: "1"
            memory: "4Gi"
          limits:
            cpu: "2"
            memory: "8Gi"
        volumeMounts:
        - name: data
          mountPath: /data
      volumes:
      - name: data
        emptyDir:
          sizeLimit: 50Gi
---
apiVersion: v1
kind: Service
metadata:
  name: iggy-server
spec:
  selector:
    app: iggy-server
  ports:
  - port: 8090
    targetPort: 8090
    name: tcp
  - port: 3000
    targetPort: 3000
    name: http
  type: ClusterIP
EOF

echo ""
echo "=== Infrastructure setup complete ==="
echo ""
echo "Export these before running the benchmark:"
echo "  export POSTGRES_CONNECTION_STRING=\"postgresql://iggy_bench:${DB_PASSWORD}@${RDS_ENDPOINT}:5432/tpch\""
echo "  export ICEBERG_WAREHOUSE=\"s3://${S3_BUCKET}/\""
echo "  export ICEBERG_CATALOG_URI=\"http://nessie.${NESSIE_NAMESPACE}.svc.cluster.local:19120/api/v1\""
echo "  export S3_ENDPOINT=\"https://s3.${REGION}.amazonaws.com\""
echo "  export AWS_REGION=\"${REGION}\""
echo "  export IGGY_SERVER_ADDRESS=\"iggy-server.default.svc.cluster.local:8090\""
