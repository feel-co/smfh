#!/usr/bin/env bash
touch outputs/delete
touch outputs/not_dir
touch outputs/modified

mkdir -p ./outputs/recursiveSymlink/dir/file_in_way
touch ./outputs/recursiveSymlink/dir/file_in_way
