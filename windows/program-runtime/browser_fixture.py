import os, json, subprocess, tempfile
os.environ['WEBCODEX_BROWSER_EXECUTABLE']=str(__import__('pathlib').Path(__import__('sys').executable).parent/'browser/headless_shell.exe')
os.environ['NODE_OPTIONS']='--preserve-symlinks --preserve-symlinks-main'
os.environ['PLAYWRIGHT_BROWSERS_PATH']=str(__import__('pathlib').Path(__import__('sys').executable).parent/'browser')
print('browser-fixture: importing',flush=True)
os.environ['DEBUG']='pw:browser*'
try:
    probe=subprocess.run([os.environ['WEBCODEX_BROWSER_EXECUTABLE'],'--headless','--enable-logging=stderr','--v=1','--single-process','--in-process-gpu','--disable-crashpad-for-testing','--no-sandbox','--disable-gpu','--no-proxy-server','--no-first-run','--user-data-dir='+tempfile.mkdtemp(),'--dump-dom','http://127.0.0.1:'+os.environ['WEBCODEX_PORT']+'/ui'],capture_output=True,text=True,encoding='utf-8',errors='replace',timeout=10)
except subprocess.TimeoutExpired as error:
    print('direct timeout diagnostic',error.stderr,flush=True)
    raise
print('browser-direct-probe',probe.returncode,probe.stdout[-300:],probe.stderr[-12000:],flush=True)
from playwright.sync_api import sync_playwright
print('browser-fixture: starting driver',flush=True)
with sync_playwright() as p:
    print('browser-fixture: launching Edge',flush=True)
    browser=p.chromium.launch(executable_path=os.environ['WEBCODEX_BROWSER_EXECUTABLE'],headless=True,args=['--no-sandbox','--disable-gpu','--disable-background-networking','--no-proxy-server'])
    context=browser.new_context(service_workers='block',accept_downloads=False)
    print('browser-fixture: opening page',flush=True)
    page=context.new_page()
    page.goto('http://127.0.0.1:'+os.environ['WEBCODEX_PORT']+'/ui',wait_until='domcontentloaded')
    page.locator('#name').fill('sandbox')
    page.locator('#send').click()
    assert page.locator('#result').inner_text()=='Hello sandbox'
    page.screenshot(path='browser-evidence.png')
    print(json.dumps({'framework_browser_fixture':True,'title':page.title(),'result':page.locator('#result').inner_text(),'engine_version':browser.version}))
    context.close()
    browser.close()
