# Package name is the sudo_py_library target name (tree artifact directory).
# __init__.py re-exports the entry module `greet` (from entry = "greet.sudo").
from greet_py import greet


def test_greet():
    assert greet.greet("hi") == "HI !"


if __name__ == "__main__":
    test_greet()
    print("PASSED")
