import sys, os
sys.path.insert(0, os.path.dirname(os.path.dirname(__file__)))

def test_health_returns_ok():
    from fastapi.testclient import TestClient
    from server import app
    client = TestClient(app)
    resp = client.get("/health")
    assert resp.status_code == 200
    data = resp.json()
    assert data["status"] == "ok"
    assert "uptime_s" in data
    assert isinstance(data["models"], list)

def test_compact_route_rejects_missing_text():
    from fastapi.testclient import TestClient
    from server import app
    client = TestClient(app)
    resp = client.post("/compact", json={"ratio": 0.5})
    assert resp.status_code == 422

def test_enrich_route_rejects_missing_query():
    from fastapi.testclient import TestClient
    from server import app
    client = TestClient(app)
    resp = client.post("/enrich", json={"text": "some code"})
    assert resp.status_code == 422
