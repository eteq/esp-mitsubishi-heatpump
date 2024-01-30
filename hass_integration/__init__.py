from homeassistant.core import HomeAssistant
from homeassistant.helpers.typing import ConfigType

DOMAIN = 'esp_mitsubishi_heatpump'


async def async_setup(hass: HomeAssistant, config: ConfigType) -> bool:
    # Data that you want to share with your platforms
    hass.data[DOMAIN] = { }

    hass.helpers.discovery.load_platform('climate', DOMAIN, {}, config)

    return True