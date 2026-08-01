$ErrorActionPreference = 'Stop'
python -m pip install -r requirements.txt
python -m unittest -v test_audio_gui.py
python -m PyInstaller --noconfirm --clean --onefile --windowed `
  --name 'Audio Config Manager' `
  --collect-all winappaudiorouter `
  --collect-all pycaw `
  --collect-all comtypes `
  audio_gui.py

