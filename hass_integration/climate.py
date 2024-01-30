import logging

_LOGGER = logging.getLogger(__name__)

from homeassistant.const import UnitOfTemperature, PRECISION_HALVES
from homeassistant.core import HomeAssistant
from homeassistant.helpers.entity_platform import AddEntitiesCallback
from homeassistant.helpers.typing import ConfigType, DiscoveryInfoType
from homeassistant.components import climate, zeroconf

from zeroconf import ServiceBrowser, ServiceStateChange

import requests

from . import DOMAIN


async def async_setup_platform(
    hass: HomeAssistant,
    config: ConfigType,
    async_add_entities: AddEntitiesCallback,
    discovery_info: DiscoveryInfoType | None = None
) -> None:
    """Set up the sensor platform."""
    # We only want this platform to be set up via discovery.
    if discovery_info is None:
        return

    def on_service_state_change(zeroconf, service_type, name, state_change):
        if state_change == ServiceStateChange.Added:

            info = zeroconf.get_service_info(service_type, name)
            addrs = info.parsed_scoped_addresses()

            _LOGGER.info(f"Adding eteq-mheatpump: {name}")

            async_add_entities([MitsubishiHeatpumpController(ip=addrs[0], port=info.port, name=name)])

        elif state_change == ServiceStateChange.Removed:
            _LOGGER.warning(f"eteq-mheatpump name: {name} removed.  Can't remove from hass.")
        elif state_change == ServiceStateChange.Updated:
            _LOGGER.warning(f"eteq-mheatpump name: {name} updated.  Not sure what to do.")
        else:
            _LOGGER.warning(f"eteq-mheatpump service state change unrecognized: {state_change}")

    zc = await zeroconf.async_get_instance(hass)
    browser = ServiceBrowser(zc, ['_eteq-mheatpump._tcp.local.'],
                             handlers=[on_service_state_change])


class MitsubishiHeatpumpController(climate.ClimateEntity):
    _attr_temperature_unit = UnitOfTemperature.CELSIUS
    _attr_precision = PRECISION_HALVES
    _attr_target_temperature_step = _attr_precision

    _attr_supported_features = (
        climate.ClimateEntityFeature.TARGET_TEMPERATURE
        | climate.ClimateEntityFeature.FAN_MODE
        | climate.ClimateEntityFeature.SWING_MODE
    )

    _attr_hvac_modes = [
        climate.HVACMode.OFF,
        climate.HVACMode.HEAT,
        climate.HVACMode.COOL,
        climate.HVACMode.DRY,
        climate.HVACMode.FAN_ONLY,
    ]
    _attr_fan_modes = [climate.FAN_AUTO,
                       climate.FAN_LOW,
                       climate.FAN_MEDIUM,
                       climate.FAN_HIGH,
                       climate.FAN_OFF
                       ]

    _attr_swing_modes = [climate.SWING_OFF,
                         climate.SWING_VERTICAL,
                         climate.SWING_HORIZONTAL,
                         climate.SWING_BOTH
                        ]

    def __init__(self, name, ip, port):
        super().__init__()  # may or may not be necessary for ClimateEntity?
        self._last_state = None
        self._attr_name = name
        self._ip = ip
        self._port = port

    async def async_update(self):
        r = requests.get(f"http://{self._ip}:{self._port}/status.json")
        if r.ok:
            j = r.json()
            _LOGGER.info(f"got state {j}")
            raise NotImplementedError()
        else:
            raise NotImplementedError()
