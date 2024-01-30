import datetime
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

            async_add_entities([MitsubishiHeatpumpController(ip=addrs[0], port=info.port, name=name)], update_before_add=True)

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

    _attr_fan_modes = ['Auto', 'Quiet', 'Low', 'Medium', 'High', ]

    _attr_swing_modes = [climate.SWING_OFF,
                         climate.SWING_VERTICAL,
                         climate.SWING_HORIZONTAL,
                         climate.SWING_BOTH
                        ]

    def __init__(self, name, ip, port):
        super().__init__()  # may or may not be necessary for ClimateEntity?
        self._last_status = None
        self._last_status_datetime = None
        self._attr_name = name
        self._attr_ip = ip
        self._attr_port = port

    @property
    def current_temperature(self):
        return self._last_status['room_temperature_c']
    
    @property
    def fan_mode(self):
        return self._last_status['fan_speed']

    @property
    def hvac_mode(self):
        if not self._last_status['poweron']:
            return climate.HVACMode.OFF
        
        mstr = self._last_status['mode']
        if mstr == 'Heat':
            return climate.HVACMode.HEAT
        elif mstr == 'Cool':
            return climate.HVACMode.COOL
        elif mstr == 'Dry':
            return climate.HVACMode.DRY
        elif mstr == 'Fan':
            return climate.HVACMode.FAN
        elif mstr == 'Auto':
            return climate.HVACMode.AUTO
        else:
            raise NotImplementedError(f"Unknown mode: {mstr}")

    @property
    def swing_mode(self):
        hswing =  self._last_status['vane'] == 'Swing'
        vswing =  self._last_status['widevane'] == 'Swing'

        if hswing and vswing:
            return climate.SWING_BOTH
        elif hswing:
            return climate.SWING_HORIZONTAL
        elif vswing:
            return climate.SWING_VERTICAL
        else:
            return climate.SWING_OFF

    @property
    def target_temperature(self):
        return self._last_status['desired_temperature_c']


    async def async_update(self):
        async with self.hass.data[DOMAIN]['aiohttp_session'] as session:
            async with session.get(f"http://{self._attr_ip}:{self._attr_port}/status.json") as resp:
                t = datetime.datetime.strptime(resp.headers['Date'],'%a, %d %b %Y %H:%M:%S GMT')
                if resp.ok:
                    self._last_status = await resp.json()
                    self._last_status_datetime = t
                else:
                    _LOGGER.warning(f"Failed to get update for heat pump "
                                    f"{self._attr_name} due to "
                                    f"{resp.status_code}: {resp.reason} "
                                    f"at time {t}")

    async def async_set_hvac_mode(self, hvac_mode):
        """Set new target hvac mode."""
        raise NotImplementedError()

    async def async_set_fan_mode(self, fan_mode):
        """Set new target fan mode."""
        raise NotImplementedError()

    async def async_set_swing_mode(self, swing_mode):
        """Set new target swing operation."""
        raise NotImplementedError()

    async def async_set_temperature(self, **kwargs):
        """Set new target temperature."""
        raise NotImplementedError()