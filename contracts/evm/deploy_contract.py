import os
import subprocess
import sys

def main():
    # Load .env file
    env_path = os.path.abspath(os.path.join(os.path.dirname(__file__), ".env"))
    if not os.path.exists(env_path):
        env_path = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", "..", ".env"))
    
    print(f"Loading environment from: {env_path}")
    
    env_vars = os.environ.copy()
    if os.path.exists(env_path):
        with open(env_path, "r", encoding="utf-8") as f:
            for line in f:
                line = line.strip()
                if not line or line.startswith("#"):
                    continue
                if "=" in line:
                    key, val = line.split("=", 1)
                    key = key.strip()
                    val = val.strip().strip("'").strip('"')
                    env_vars[key] = val
                    
    # Print loaded values (mask private key)
    print("Loaded Env:")
    print(f"  ETH_HTTP_URL: {env_vars.get('ETH_HTTP_URL')}")
    pk = env_vars.get('PRIVATE_KEY', '')
    masked_pk = pk[:6] + "..." + pk[-4:] if len(pk) > 10 else "not set"
    print(f"  PRIVATE_KEY: {masked_pk}")
    
    # Run forge command
    # If the user passed arguments to the script, pass them to forge script
    args = sys.argv[1:]
    cmd = ["forge", "script", "script/Deploy.s.sol"]
    
    # Check if --rpc-url is already specified
    has_rpc = False
    for arg in args:
        if arg.startswith("--rpc-url"):
            has_rpc = True
            break
            
    if not has_rpc:
        rpc_url = env_vars.get("ETH_HTTP_URL", "")
        if rpc_url:
            cmd += ["--rpc-url", rpc_url]
            
    cmd += args
        
    print(f"Running command: {' '.join(cmd)}")
    
    try:
        # Run process in real-time
        res = subprocess.run(cmd, env=env_vars, shell=True)
        sys.exit(res.returncode)
    except Exception as e:
        print(f"Error running command: {e}")
        sys.exit(1)

if __name__ == "__main__":
    main()
