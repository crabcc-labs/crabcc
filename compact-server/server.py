import time
import uvicorn
from fastapi import FastAPI
from pydantic import BaseModel

import compress
import enrich as enrich_mod

app = FastAPI(title="compact-server")
_start = time.time()

class CompactRequest(BaseModel):
    text: str
    ratio: float = 0.5

class CompactResponse(BaseModel):
    compressed: str
    original_tokens: int
    compressed_tokens: int

class EnrichRequest(BaseModel):
    text: str
    query: str

class EnrichResponse(BaseModel):
    plan: str

@app.post("/compact", response_model=CompactResponse)
def route_compact(req: CompactRequest) -> CompactResponse:
    result = compress.compact(req.text, req.ratio)
    return CompactResponse(**result)

@app.post("/enrich", response_model=EnrichResponse)
def route_enrich(req: EnrichRequest) -> EnrichResponse:
    plan = enrich_mod.enrich(req.text, req.query)
    return EnrichResponse(plan=plan)

@app.get("/health")
def route_health():
    return {
        "status": "ok",
        "models": compress.loaded_models() + enrich_mod.loaded_models(),
        "uptime_s": int(time.time() - _start),
    }

def main():
    port = int(__import__("os").environ.get("COMPACT_PORT", "8080"))
    uvicorn.run("server:app", host="0.0.0.0", port=port, reload=False)

if __name__ == "__main__":
    main()
