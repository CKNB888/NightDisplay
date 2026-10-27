import os, sys, tempfile
sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))
os.environ.setdefault("NIGHTDISPLAY_HOME", tempfile.mkdtemp(prefix="ndtest-"))
