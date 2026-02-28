import json
import os
import zipfile
from jsf import JSF

SCHEMAS_DIR = "/tmp/agent-life-data-format/schemas"
FIXTURES_DIR = "alf-cli/fixtures"
OUTPUT_ALF = os.path.join(FIXTURES_DIR, "synthetic-agent.alf")

def load_schema(name):
    with open(os.path.join(SCHEMAS_DIR, name)) as f:
        return json.load(f)

def generate_data(schema):
    return JSF(schema).generate()

def main():
    os.makedirs(FIXTURES_DIR, exist_ok=True)
    
    manifest_schema = load_schema("manifest.schema.json")
    identity_schema = load_schema("identity.schema.json")
    principals_schema = load_schema("principals.schema.json")
    credentials_schema = load_schema("credentials.schema.json")
    attachments_schema = load_schema("attachments.schema.json")
    memory_record_schema = load_schema("memory-record.schema.json")
    
    manifest_data = generate_data(manifest_schema)
    identity_data = generate_data(identity_schema)
    principals_data = generate_data(principals_schema)
    credentials_data = generate_data(credentials_schema)
    attachments_data = generate_data(attachments_schema)
    memory_records = [generate_data(memory_record_schema) for _ in range(10)]
    
    # 1. Align IDs
    agent_id = manifest_data.get("agent", {}).get("id", "00000000-0000-0000-0000-000000000000")
    if "agent" not in manifest_data:
        manifest_data["agent"] = {"id": agent_id, "name": "Agent", "source_runtime": "test_runtime"}
    if not manifest_data["agent"].get("name"):
        manifest_data["agent"]["name"] = "Agent Name"
    if not manifest_data["agent"].get("source_runtime"):
        manifest_data["agent"]["source_runtime"] = "test_runtime"
    
    if "agent_id" in identity_data or True:
        identity_data["agent_id"] = agent_id
        
    if "structured" in identity_data and "sub_agents" in identity_data["structured"]:
        for sa in identity_data["structured"]["sub_agents"]:
            if "name" in sa and not sa["name"]:
                sa["name"] = "Sub-agent Name"
                
    for p in principals_data.get("principals", []):
        if "profile" in p:
            p["profile"]["principal_id"] = p.get("id")
            
    # 2. Fix empty strings
    for c in credentials_data.get("credentials", []):
        if "encrypted_payload" in c and not c["encrypted_payload"]:
            c["encrypted_payload"] = "a"
        if "encryption" in c:
            if "nonce" in c["encryption"] and not c["encryption"]["nonce"]:
                c["encryption"]["nonce"] = "a"
            if "algorithm" in c["encryption"] and not c["encryption"]["algorithm"]:
                c["encryption"]["algorithm"] = "a"
    
    # 3. Fix memory records
    for r in memory_records:
        r["agent_id"] = agent_id
        if "namespace" in r and not r["namespace"]:
            r["namespace"] = "default"
        if "source" in r and "runtime" in r["source"] and not r["source"]["runtime"]:
            r["source"]["runtime"] = "runtime"
            
        # Fix embeddings lengths
        if "embeddings" in r:
            for emb in r["embeddings"]:
                emb["vector"] = [0.1] * emb.get("dimensions", 1)
                emb["dimensions"] = len(emb["vector"])
    
    # 4. Fix manifest layers
    if "layers" not in manifest_data:
        manifest_data["layers"] = {}
        
    manifest_data["layers"].setdefault("identity", {})["file"] = "identity.json"
    manifest_data["layers"]["identity"]["version"] = identity_data.get("version", 1)
    manifest_data["layers"].setdefault("principals", {})["file"] = "principals.json"
    manifest_data["layers"]["principals"]["count"] = len(principals_data.get("principals", []))
    manifest_data["layers"].setdefault("credentials", {})["file"] = "credentials.json"
    manifest_data["layers"]["credentials"]["count"] = len(credentials_data.get("credentials", []))
    manifest_data["layers"].setdefault("attachments", {})["file"] = "attachments.json"
    manifest_data["layers"]["attachments"]["count"] = len(attachments_data.get("attachments", []))
    manifest_data["layers"]["attachments"]["included_count"] = 0
    manifest_data["layers"]["attachments"]["included_size_bytes"] = 0
    manifest_data["layers"]["attachments"]["referenced_count"] = 0
    manifest_data["layers"]["attachments"]["referenced_size_bytes"] = 0
    manifest_data["layers"].setdefault("memory", {})["index_file"] = "memory/index.json"
    manifest_data["alf_version"] = "1.0.0"
    
    partition_file = "memory/partitions/2026-Q1.jsonl"
    manifest_data["layers"]["memory"]["partitions"] = [
        {
            "file": partition_file,
            "from": "2026-01-01",
            "to": "2026-03-31",
            "record_count": len(memory_records),
            "sealed": False
        }
    ]
    manifest_data["layers"]["memory"]["record_count"] = len(memory_records)
    
    memory_index_data = {
        "partitions": manifest_data["layers"]["memory"]["partitions"]
    }
    
    # Get version from schema_version.txt
    try:
        with open(os.path.join(FIXTURES_DIR, "schema_version.txt")) as f:
            schema_version = f.read().strip()
    except FileNotFoundError:
        schema_version = "unknown"
        
    print(f"Creating archive at {OUTPUT_ALF} (schema version: {schema_version})...")
    with zipfile.ZipFile(OUTPUT_ALF, 'w', zipfile.ZIP_DEFLATED) as zf:
        zfile_write = lambda name, data: zf.writestr(name, json.dumps(data, indent=2))
        zfile_write("manifest.json", manifest_data)
        zfile_write("identity.json", identity_data)
        zfile_write("principals.json", principals_data)
        zfile_write("credentials.json", credentials_data)
        zfile_write("attachments.json", attachments_data)
        zfile_write("memory/index.json", memory_index_data)
        
        jsonl_content = "\n".join(json.dumps(r) for r in memory_records) + "\n"
        zf.writestr(partition_file, jsonl_content)
        
        zf.writestr("raw/openclaw/.keep", "")
        zf.writestr("artifacts/.keep", "")
        
    print("Done!")

if __name__ == "__main__":
    main()
