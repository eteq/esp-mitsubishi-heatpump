import re
import time
import asyncio
import aiohttp
import logging
from collections import defaultdict
from datetime import timedelta

_LOGGER = logging.getLogger(__name__)

from homeassistant.config_entries import ConfigEntry
from homeassistant.const import UnitOfTemperature, PRECISION_HALVES, ATTR_TEMPERATURE
from homeassistant.core import HomeAssistant
from homeassistant.helpers.entity_platform import AddEntitiesCallback
from homeassistant.components import climate
from homeassistant.const import CONF_HOST, CONF_NAME, CONF_PORT, CONF_MAC

from . import DOMAIN


SCAN_INTERVAL = timedelta(seconds=10)
CONTROLLER_SEND_WAIT_TIME_SECS = 0.02 # 20ms should be enough?

async def async_setup_entry(
    hass: HomeAssistant,
    config_entry: ConfigEntry,
    async_add_entities: AddEntitiesCallback,
) -> None:
    """Set up the Mitsubishi Heat Pump climate platform."""
    async_add_entities([MitsubishiHeatpumpController(name=config_entry.data[CONF_NAME],
                                                     ip=config_entry.data[CONF_HOST],
                                                     port=config_entry.data[CONF_PORT],
                                                     mac=config_entry.data[CONF_MAC])])


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

    _attr_fan_modes = ['Auto', 'Quiet', 'Low', 'Medium', 'High']

    _attr_swing_modes = [climate.SWING_OFF,
                         climate.SWING_VERTICAL,
                         climate.SWING_HORIZONTAL,
                         climate.SWING_BOTH
                        ]

    def __init__(self, name, ip, port, mac):
        super().__init__()  # may or may not be necessary for ClimateEntity?

        self._attr_unique_id = mac
        self._last_status = defaultdict(lambda:None)
        self._last_status_time = None
        self._attr_name = name
        self._attr_ip = ip
        self._attr_port = port
        self._queued_settings = {}
        self._attr_available = False

    @property
    def last_status(self):
        return self._last_status

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
            return climate.HVACMode.FAN_ONLY
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

    async def async_set_hvac_mode(self, hvac_mode):
        """Set new target hvac mode."""
        if hvac_mode == climate.HVACMode.OFF:
            self._queued_settings['poweron'] = False
        elif hvac_mode == climate.HVACMode.HEAT:
            self._queued_settings['poweron'] = True
            self._queued_settings['mode'] = 'Heat'
        elif hvac_mode == climate.HVACMode.COOL:
            self._queued_settings['poweron'] = True
            self._queued_settings['mode'] = 'Cool'
        elif hvac_mode == climate.HVACMode.DRY:
            self._queued_settings['poweron'] = True
            self._queued_settings['mode'] = 'Dry'
        elif hvac_mode == climate.HVACMode.FAN_ONLY:
            self._queued_settings['poweron'] = True
            self._queued_settings['mode'] = 'Fan'
        elif hvac_mode == climate.HVACMode.AUTO:
            self._queued_settings['poweron'] = True
            self._queued_settings['mode'] = 'Auto'
        else:
            raise ValueError(f"unrecognized hvac_mode {hvac_mode}")

        self.set_changes_pending()

    async def async_set_fan_mode(self, fan_mode):
        """Set new target fan mode."""
        if fan_mode == 'Auto':
            self._queued_settings['fan_speed'] = 'Auto'
        elif fan_mode == 'Quiet':
            self._queued_settings['fan_speed'] = 'Quiet'
        elif fan_mode == 'Low':
            self._queued_settings['fan_speed'] = 'Low'
        elif fan_mode == 'Medium':
            self._queued_settings['fan_speed'] = 'Med'
        elif fan_mode == 'High':
            self._queued_settings['fan_speed'] = 'High'
        elif fan_mode == 'Powerful':
            self._queued_settings['fan_speed'] = 'VeryHigh'
        else:
            raise ValueError(f"unrecognized fan_mode {fan_mode}")

        self.set_changes_pending()

    async def async_set_swing_mode(self, swing_mode):
        """Set new target swing operation."""
        if swing_mode == climate.SWING_OFF:
            self._queued_settings['vane'] = 'Auto'
            self._queued_settings['widevane'] = 'Mid'
        elif swing_mode == climate.SWING_VERTICAL:
            self._queued_settings['vane'] = 'Swing'
            if self._last_status['widevane'] == 'Swing':
                self._queued_settings['widevane'] = 'Mid'
        elif swing_mode == climate.SWING_HORIZONTAL:
            self._queued_settings['widevane'] = 'Swing'
            if self._last_status['vane'] == 'Swing':
                self._queued_settings['vane'] = 'Auto'
        elif swing_mode == climate.SWING_BOTH:
            self._queued_settings['vane'] = 'Swing'
            self._queued_settings['widevane'] = 'Swing'
        else:
            raise ValueError(f"unrecognized swing_mode {swing_mode}")

        self.set_changes_pending()

    async def async_set_temperature(self, **kwargs):
        """Set new target temperature."""
        temp = kwargs.get(ATTR_TEMPERATURE)

        self._queued_settings['desired_temperature_c'] = float(temp)

        self.set_changes_pending()


    async def async_update(self):
        if self._queued_settings:
            await self.send_changes()
            await asyncio.sleep(CONTROLLER_SEND_WAIT_TIME_SECS)
        url = f"http://{self._attr_ip}:{self._attr_port}/status.json"

        try:
            async with self.hass.data[DOMAIN]['aiohttp_session'].get(url) as resp:
                if resp.ok:
                    self._last_status = await resp.json()
                    self._last_status_time = time.time()
                    self._attr_available = True
                else:
                    text = await resp.text()
                    _LOGGER.warning(f"Failed to get update for heat pump "
                                    f"{self._attr_name} due to "
                                    f"{resp.status}: {resp.reason}. "
                                    f"Content: {text}")
                    self._attr_available = False
        except aiohttp.ClientError as e:
            _LOGGER.warning(f"Failed to get update for heat pump {self._attr_name} due to {e}")
            self._attr_available = False

    async def send_changes(self):
        if not self._queued_settings:
            _LOGGER.info("no changes remaining to be sent on mhc, but request was made... ignoring.")
            return

        _LOGGER.info("sending a changeset on mhc")

        data_to_send = {k:None for k in ['poweron', 'mode', 'desired_temperature_c', 'fan_speed', 'vane', 'widevane']}
        data_to_send.update(self._queued_settings)

        url = f"http://{self._attr_ip}:{self._attr_port}/set.json"
        try:
            async with self.hass.data[DOMAIN]['aiohttp_session'].post(url, json=data_to_send) as resp:
                if resp.ok:
                    # only clear an entry if it matches tthe current expected target, since updates could have happened while we were waiting for the send to finish
                    for k,v in (await resp.json()).items():
                        if k in self._queued_settings and v == self._queued_settings[k]:
                            del self._queued_settings[k]

                    if self._queued_settings:
                        _LOGGER.warning(f"Some settings did not get updated in the last send: {list(self._queued_settings)} will try again next update")
                else:
                    text = await resp.text()
                    _LOGGER.warning(f"Failed to send changeset {data_to_send} to heat pump "
                                    f"{self._attr_name} due to "
                                    f"{resp.status}: {resp.reason}. "
                                    f"Content: {text}")
                    self._attr_available = False
        except aiohttp.ClientError as e:
            _LOGGER.warning(f"Failed to send update for heat pump {self._attr_name} due to {e}")
            self._attr_available = False

    def set_changes_pending(self):
        self.schedule_update_ha_state(True)
