from setuptools import setup
from setuptools_rust import Binding, RustExtension

setup(
    name="pyfacet",
    version="0.1.0",
    rust_extensions=[RustExtension("pyfacet.pyfacet", binding=Binding.PyO3)],
    packages=["pyfacet"],
    # rust extensions are not zip safe, just like C-extensions.
    zip_safe=False,
    install_requires=["progressbar"],
)
