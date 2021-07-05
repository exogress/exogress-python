import exogress
import threading
import logging
from aiohttp import web
import time

LOGGER_NAME = 'exogress'

logger = logging.getLogger()
logger.setLevel(logging.INFO)
formatter = logging.Formatter('%(asctime)s : %(levelname)s : %(name)s : %(message)s')
terminal = logging.StreamHandler()
terminal.setFormatter(formatter)
logger.addHandler(terminal)

logging.info("serving on 4000")

instance = exogress.Instance(
    access_key_id="",
    secret_access_key="",
    account="",
    project="",
    watch_config=True,
    config_path="./Exofile.yml",
    labels={
        "label1": "val1",
    },
)


def spawn_instance():
    logger.info("exogress thread spawned!")
    instance.spawn()


exogressThread = threading.Thread(target=spawn_instance)
exogressThread.start()


# time.sleep(5)
# print("reload")
# instance.reload()
# time.sleep(5)
# print("stop")
# instance.stop()

async def handle(request):
    return web.Response(text="Hello from exogress on Python")


app = web.Application()
app.router.add_get('/', handle)

web.run_app(app, port=4000)
