Clear Windows file history files by finding files with the same name except for a UTC timestamp
within the same directory and keeping the latest version of the file, trimming the timestamp
from the file name (unless disabled) and moving all other copies of the file to a to_delete
folder or deleting them instantly (depending on whether --purge is set).

For example, consider this file hierarchy:

 - dir
   - dir2
     - File2 (2016_06_22 13_39_28 UTC).jpg
     - File2 (2017_06_22 13_39_28 UTC).jpg
   - File1 (2016_06_22 13_39_28 UTC).jpg
   - File1 (2017_06_22 13_39_28 UTC).jpg

Will turn into this (unless the purge option is enabled, in which case duplicate files are deleted instantly):

 - dir
   - dir2
     - File2.jpg
   - File1.jpg
   - fhcleanup_to_del # this directly is only created if purge is not enabled, else the files in this dir are deleted instead
     - dir2
       - File2 (2016_06_22 13_39_28 UTC).jpg
     - File1 (2016_06_22 13_39_28 UTC).jpg