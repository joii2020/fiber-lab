"""Validate RPC auth, process identity and configuration without launching a chain."""
import importlib.util
import json
from pathlib import Path
import subprocess
import tempfile
import threading
import unittest
from http.server import BaseHTTPRequestHandler, HTTPServer

spec = importlib.util.spec_from_file_location('lab', Path(__file__).resolve().parents[1] / 'scripts/lab.py')
lab = importlib.util.module_from_spec(spec)
spec.loader.exec_module(lab)


class LabTests(unittest.TestCase):
    def test_rpc_auth_and_error(self):
        received = []

        class Handler(BaseHTTPRequestHandler):
            def do_POST(self):
                request = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
                received.append((self.headers.get('Authorization'), request))
                result = {'error': {'code': -1, 'message': 'rejected'}} if request['method'] == 'bad' else {'result': {'ok': True}}
                self.send_response(200)
                self.end_headers()
                self.wfile.write(json.dumps(result).encode())

            def log_message(self, *args):
                pass

        server = HTTPServer(('127.0.0.1', 0), Handler)
        thread = threading.Thread(target=server.serve_forever)
        thread.start()
        try:
            url = f'http://127.0.0.1:{server.server_port}'
            self.assertEqual(lab.rpc(url, 'node_info', token='fixture'), {'ok': True})
            self.assertEqual(received[0][0], 'Bearer fixture')
            with self.assertRaises(RuntimeError):
                lab.rpc(url, 'bad')
        finally:
            server.shutdown()
            thread.join()
            server.server_close()

    def test_stop_only_owned_process(self):
        with tempfile.TemporaryDirectory() as tmp:
            child = subprocess.Popen(['sleep', '60'])
            try:
                state = Path(tmp)
                # Incorrect creation time must not identify this PID as ours.
                lab.save(state, [{'pid': child.pid, 'start_time': 'incorrect'}])
                lab.stop_nodes(state)
                self.assertIsNone(child.poll())
                lab.save(state, [{'pid': child.pid, 'start_time': lab.identity(child.pid)}])
                lab.stop_nodes(state)
                child.wait(timeout=2)
                self.assertEqual(lab.records(state), [])
            finally:
                if child.poll() is None:
                    child.terminate()
                    child.wait()

    def test_profile_and_fixture(self):
        profile = lab.load('devnet')
        self.assertEqual([n['id'] for n in profile['nodes']], [1, 2])
        self.assertTrue(lab.token(profile['nodes'][1]))
        self.assertNotEqual(lab.environment(Path('/tmp/lab'))['XDG_DATA_HOME'], '/home/joii/.local/share')


if __name__ == '__main__':
    unittest.main()
