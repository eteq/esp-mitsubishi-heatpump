# esp_mitsubishi_heatpump

1. Compile the rust firmware and flash it onto your esp32cX's. Set ``WIFI_SSID`` AND ``WIFI_PASS`` environment variables to your local wifi network. You can also set ``TX_PIN``/``RX_PIN`` to set the pins to talk to the heatpump, although the default of 4/5 is known to work well.
2. Connect the esp32cX's to the CN105 connector
3. Repeat for all heat pumps you have
4. Install the home assistant integration (see hass_integration/README.md for more.)
5. Profit!

For more on the hardware details (step 2), see https://chrdavis.github.io/hacking-a-mitsubishi-heat-pump-Part-1/ . Note that for me it worked to just connect the 5V on CN105  directly to the esp32cX as well as the TX/RX lines without any level shifters.  This is probably hardware-dependent though.

Alternatively/additionally, you can do just 1-3 and directly connect to the controllers on your local network to control the heat pumps. ``http://heatpump-controller-{MAC ADDRESS}.local:8923/index.html`` should do the job.