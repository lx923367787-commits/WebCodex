"""Prepare a credential-free Python standard-library resource for agent_process.

Reads an installed Python distribution, never user site-packages or login state.
Writes a new immutable staging directory; does not edit Runner configuration,
grant permissions, start a business program, or call a model.
"""
from __future__ import annotations
import argparse
import hashlib
import json
from pathlib import Path
import sys
import shutil
import zipfile


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def plain(path, *, directory=False):
    if not path.is_absolute():
        raise ValueError("absolute_path_required")
    for node in [*reversed(path.parents), path]:
        info=node.lstat()
        if node.is_symlink() or getattr(info,"st_file_attributes",0)&0x400:
            raise ValueError("linked_resource_path_rejected")
    if directory != path.is_dir():
        raise ValueError("resource_type_mismatch")
    if not directory and path.stat().st_nlink != 1:
        raise ValueError("hardlinked_resource_rejected")


def prepare(python_root:Path,output:Path,browser_root:Path|None=None):
    plain(python_root,directory=True)
    if output.exists():raise ValueError("new_output_directory_required")
    plain(output.parent,directory=True)
    executable=python_root/"python.exe";plain(executable)
    candidates=list(python_root.glob("python3[0-9]*.dll"))
    if len(candidates)!=1:raise ValueError("python_abi_ambiguous")
    abi=candidates[0].stem
    library=python_root/"Lib";plain(library,directory=True)
    output.mkdir()
    files=[]
    def add(path,destination):
        plain(path)
        files.append({"source":str(path),"destination":destination,"sha256":sha(path)})
    for path in sorted(python_root.glob("*.dll")):
        add(path,"bin/"+path.name)
    dlls=python_root/"DLLs";plain(dlls,directory=True)
    for path in sorted(dlls.iterdir()):
        if path.suffix.lower() in (".dll",".pyd"):add(path,"bin/DLLs/"+path.name)
    archive=output/(abi+".zip")
    count=0
    with zipfile.ZipFile(archive,"x",compression=zipfile.ZIP_DEFLATED) as bundle:
        for path in sorted(library.rglob("*.py")):
            relative=path.relative_to(library)
            if any(part in {"site-packages","__pycache__","test","tests","ensurepip","idlelib","tkinter"} for part in relative.parts):continue
            plain(path)
            info=zipfile.ZipInfo(relative.as_posix(),(1980,1,1,0,0,0));info.compress_type=zipfile.ZIP_DEFLATED
            bundle.writestr(info,path.read_bytes());count+=1
    if count<100:raise ValueError("python_stdlib_incomplete")
    add(archive,"bin/"+archive.name)
    pth=output/(abi+"._pth")
    pth.write_text(f"{abi}.zip\n.\nDLLs\n../workspace\n",encoding="utf-8")
    add(pth,"bin/"+pth.name)
    # The native loader may select either the versioned DLL or executable name.
    program_pth=output/"program._pth";program_pth.write_bytes(pth.read_bytes());add(program_pth,"bin/program._pth")
    if browser_root is not None:
        plain(browser_root,directory=True)
        staged=output/"browser-runtime";staged.mkdir()
        def stage(source,relative):
            # Edge's installed static runtime uses resiliency hardlinks. Copy
            # only selected runtime files into a new, single-link staging tree;
            # execution still enforces strict pins and hashes on that tree.
            for node in [*reversed(source.parents),source]:
                info=node.lstat()
                if node.is_symlink() or getattr(info,"st_file_attributes",0)&0x400:raise ValueError("linked_runtime_path_rejected")
            destination=staged/relative;destination.parent.mkdir(parents=True,exist_ok=True)
            if destination.exists():raise ValueError("runtime_staging_collision")
            shutil.copyfile(source,destination)
            add(destination,"bin/"+relative.as_posix())
        for package in ("playwright","greenlet","pyee"):
            source=library/"site-packages"/package;plain(source,directory=True)
            for path in sorted(source.rglob('*')):
                if not path.is_file() or any(p in {"__pycache__","tests"} for p in path.relative_to(source).parts):continue
                if path.suffix in {".pyc",".pyo"}:continue
                if package=="greenlet" and path.suffix not in {".py",".pyd"} and not path.name.startswith("LICENSE"):continue
                stage(path,Path(package)/path.relative_to(source))
        optional=library/"site-packages"/"typing_extensions.py"
        if optional.is_file():stage(optional,Path(optional.name))
        for path in sorted(browser_root.iterdir()):
            if path.is_file() and (path.suffix.lower() in {".dll",".pak",".manifest"} or path.name in {"msedge.exe","icudtl.dat","v8_context_snapshot.bin"}):
                stage(path,Path("browser")/browser_root.name/path.name)
        stage(browser_root/"msedge.exe",Path("browser/msedge.exe"))
        for locale in ("en-US.pak","zh-CN.pak"):
            stage(browser_root/"Locales"/locale,Path("browser")/browser_root.name/"Locales"/locale)
    if len(files)>=512:raise ValueError("python_resource_file_limit")
    profile={"id":"python-browser" if browser_root else "python-stdlib","executable":str(executable),"executable_sha256":sha(executable),"files":files}
    result={"profiles":[profile]}
    (output/"resources.json").write_text(json.dumps(result,ensure_ascii=False,indent=2),encoding="utf-8")
    (output/"preparation.json").write_text(json.dumps({"python_root":str(python_root),"browser_runtime":str(browser_root) if browser_root else None,"stdlib_modules":count,"resource_files":len(files),"credentials_copied":False,"site_packages_copied":["playwright","greenlet","pyee","typing_extensions"] if browser_root else [],"execution_performed":False},ensure_ascii=False,indent=2),encoding="utf-8")
    return output/"resources.json"


if __name__=="__main__":
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--python-root",type=Path,default=Path(sys.base_prefix))
    parser.add_argument("--output",type=Path,required=True)
    parser.add_argument("--browser-root",type=Path,help="Optional exact installed Edge version directory; no user browser profile is copied")
    options=parser.parse_args()
    print(prepare(options.python_root,options.output,options.browser_root))
